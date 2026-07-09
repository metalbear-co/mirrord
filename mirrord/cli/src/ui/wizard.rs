//! Config-wizard endpoints for the `mirrord ui` server.
//!
//! These back the wizard page (served at `/wizard`): they enumerate the namespaces and targets in
//! the user's cluster so the frontend can build a mirrord config file. They were previously served
//! by the standalone `mirrord wizard` server; now they live behind the shared `mirrord ui` server,
//! under `/api/v1`, gated by the same token auth as every other route.

use axum::{
    Router,
    extract::{Path, Query, State},
    routing::get,
};
use futures::{StreamExt, TryStreamExt};
use itertools::Itertools;
use k8s_openapi::api::{
    apps::v1::{Deployment, ReplicaSet, StatefulSet},
    batch::v1::{CronJob, Job},
    core::v1::{Container, Namespace, Pod, Service},
};
use kube::{Client, runtime::reflector::Lookup};
use mirrord_config::target::TargetType;
use mirrord_kube::{
    api::kubernetes::{rollout::Rollout, seeker::KubeResourceSeeker},
    error::KubeApiError,
};
use serde::{Deserialize, Serialize};

use crate::ui::{
    error::ApiError,
    server::{AppState, client_for_context},
};

/// Alias for the return type of the wizard route handlers.
type WizardResult<T> = Result<T, ApiError>;

/// Routes for `/api/v1/{...}` that back the config wizard page.
///
/// - `GET /is-returning`: whether the user has used the wizard before;
/// - `GET /cluster-details`: namespaces and targetable resource types in the cluster;
/// - `GET /namespace/{namespace}/targets`: targets (with detected ports) in a namespace.
pub(super) fn wizard_router() -> Router<AppState> {
    Router::new()
        .route("/is-returning", get(is_returning))
        .route("/cluster-details", get(cluster_details))
        .route("/namespace/{namespace}/targets", get(list_targets))
}

/// Returns whether the user has used the wizard enough times to be considered returning, as a bare
/// `"true"`/`"false"` string (the shape the frontend expects).
async fn is_returning(State(state): State<AppState>) -> String {
    state
        .user_data
        .lock()
        .await
        .is_returning_wizard()
        .to_string()
}

/// ### Response for `/api/v1/cluster-details`
///
/// Namespaces and target_types that mirrord can target in the user's cluster.
#[derive(Debug, Serialize)]
struct ClusterDetails {
    namespaces: Vec<String>,
    target_types: Vec<TargetType>,
}

/// Represents the relevant information for each available target returned by the
/// `/api/v1/namespace/{namespace}/targets` endpoint.
#[derive(Debug, Serialize)]
struct TargetInfo {
    /// ### Shown in the wizard's `Target` tab
    target_path: String,
    /// ### Shown in the wizard's `Target` tab
    target_namespace: String,
    /// ### Shown in the wizard's `Target` tab
    containers: Vec<String>,
    /// ### Shown in the wizard's `Network` tab
    ///
    /// Ports exposed by the target's Pod or Pods that we have auto-detected.
    ///
    /// These are the ports the user is most likely to be interested in.
    detected_ports: Vec<u16>,
}

impl TargetInfo {
    /// Create a new [`Self`], without having to construct the `target_path` manually
    fn new(
        target_type: TargetType,
        target_name: String,
        target_namespace: String,
        containers: Vec<String>,
        detected_ports: Vec<u16>,
    ) -> Self {
        Self {
            target_path: format!("{target_type}/{target_name}"),
            target_namespace,
            containers,
            detected_ports,
        }
    }
}

fn container_names(containers: &[Container]) -> Vec<String> {
    containers
        .iter()
        .map(|container| container.name.clone())
        .collect()
}

fn detected_ports(containers: &[Container]) -> Vec<u16> {
    containers
        .iter()
        .flat_map(|container| container.ports.clone().unwrap_or_default())
        .map(|port| port.container_port.unsigned_abs() as u16)
        .collect()
}

#[derive(Debug, Deserialize)]
struct Params {
    target_type: Option<TargetType>,
    /// Kube context to query. When absent, the kubeconfig's current context is used. The old
    /// standalone `mirrord wizard` took this as a CLI flag; the shared server outlives any single
    /// invocation, so the frontend selects it per request instead.
    context: Option<String>,
}

/// Returns cluster details that are shown while the user selects a target. Also flags the user as a
/// returning wizard user, since reaching this endpoint means they started the config flow.
#[tracing::instrument(level = tracing::Level::TRACE, skip_all, ret)]
async fn cluster_details(
    Query(query_params): Query<Params>,
    State(state): State<AppState>,
) -> WizardResult<axum::Json<ClusterDetails>> {
    {
        let mut user_data = state.user_data.lock().await;
        if !user_data.is_returning_wizard() {
            // ignore failures to update
            let _ = user_data.update_is_returning_wizard().await;
        }
    }

    let client = client_for_context(query_params.context.as_deref()).await?;
    let seeker = KubeResourceSeeker {
        client: &client,
        namespace: "default",
        copy_target: true,
    };

    let namespaces = seeker
        .list_all_clusterwide::<Namespace>(None)
        .filter_map(|namespace| std::future::ready(namespace.map(|n| n.metadata.name).transpose()))
        .try_collect::<Vec<_>>()
        .await
        .map_err(KubeApiError::KubeError)?;

    Ok(axum::Json(ClusterDetails {
        namespaces,
        target_types: TargetType::all()
            .filter(|&t| t != TargetType::Targetless)
            .collect(),
    }))
}

/// Returns details for individual targets. Targets can only be listed in a single namespace, and
/// may optionally only be listed for a single [`TargetType`].
#[tracing::instrument(level = tracing::Level::TRACE, skip_all, ret)]
async fn list_targets(
    Query(query_params): Query<Params>,
    Path(namespace): Path<String>,
) -> WizardResult<axum::Json<Vec<TargetInfo>>> {
    let client = client_for_context(query_params.context.as_deref()).await?;
    let seeker = KubeResourceSeeker {
        client: &client,
        namespace: &namespace,
        copy_target: true,
    };

    // Return targets according to target type param, otherwise fetch all types (sans Targetless)
    let types_of_interest = query_params
        .target_type
        .map(|t| vec![t])
        .unwrap_or_else(|| {
            TargetType::all()
                .filter(|&t| t != TargetType::Targetless)
                .collect()
        });

    let mut targets = vec![];
    for toi in types_of_interest {
        targets.extend(match toi {
            TargetType::Deployment => {
                seeker
                    .list_all_namespaced::<Deployment>(None, None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::Pod => {
                seeker
                    .list_all_namespaced::<Pod>(None, None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::Rollout => {
                seeker
                    .list_all_namespaced::<Rollout>(None, None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::Job => {
                seeker
                    .list_all_namespaced::<Job>(None, None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::CronJob => {
                seeker
                    .list_all_namespaced::<CronJob>(None, None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::StatefulSet => {
                seeker
                    .list_all_namespaced::<StatefulSet>(None, None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::Service => {
                seeker
                    .list_all_namespaced::<Service>(None, None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::ReplicaSet => {
                seeker
                    .list_all_namespaced::<ReplicaSet>(None, None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::Targetless => vec![], // the frontend does not yet support targetless
        });
    }

    Ok(axum::Json(targets))
}

/// A helper function that bridges the gap between the types in
/// [`list_all_namespaced`][KubeResourceSeeker::list_all_namespaced] and
/// [`into_info`][IntoTargetInfo::into_info]. Emits `warn!`s when `into_info` produces an error.
async fn into_info<T: IntoTargetInfo>(
    target_type: kube::Result<T>,
    seeker: &KubeResourceSeeker<'_>,
    client: &Client,
) -> Option<TargetInfo> {
    target_type
        .ok()?
        .into_info(seeker, client)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!("{error}");
            None
        })
        .map(
            |TargetInfo {
                 target_path,
                 target_namespace,
                 containers,
                 detected_ports,
             }| TargetInfo {
                target_path,
                target_namespace,
                containers: containers.into_iter().unique().collect(),
                detected_ports: detected_ports.into_iter().unique().collect(),
            },
        )
}

/// Implementable by resources that can be targeted by mirrord and can produce a [`TargetInfo`].
trait IntoTargetInfo {
    /// Construct an instance of [`TargetInfo`] from a valid target Resource. Fetches further
    /// resources in some cases, to detect ports on the resource's pods.
    ///
    /// If fetching further resources fails, return an [`ApiError`]. If there are any empty fields
    /// in the Resource spec, return `None`.
    fn into_info(
        self,
        seeker: &KubeResourceSeeker,
        client: &Client,
    ) -> impl Future<Output = Result<Option<TargetInfo>, ApiError>>;
}

impl IntoTargetInfo for Pod {
    async fn into_info(
        self,
        _seeker: &KubeResourceSeeker<'_>,
        _client: &Client,
    ) -> Result<Option<TargetInfo>, ApiError> {
        fn into_info_option(pod: Pod) -> Option<TargetInfo> {
            let target_name = pod.name()?.to_string();
            let target_namespace = pod.namespace()?.to_string();
            let containers = pod.spec?.containers;
            let detected_ports = detected_ports(&containers);
            let containers = container_names(&containers);
            Some(TargetInfo::new(
                TargetType::Pod,
                target_name,
                target_namespace,
                containers,
                detected_ports,
            ))
        }
        Ok(into_info_option(self))
    }
}

impl IntoTargetInfo for Deployment {
    async fn into_info(
        self,
        _seeker: &KubeResourceSeeker<'_>,
        _client: &Client,
    ) -> Result<Option<TargetInfo>, ApiError> {
        fn into_info_option(deployment: Deployment) -> Option<TargetInfo> {
            let target_name = deployment.name()?.to_string();
            let target_namespace = deployment.namespace()?.to_string();
            let containers = deployment.spec?.template.spec?.containers;
            let detected_ports = detected_ports(&containers);
            let containers = container_names(&containers);
            Some(TargetInfo::new(
                TargetType::Deployment,
                target_name,
                target_namespace,
                containers,
                detected_ports,
            ))
        }
        Ok(into_info_option(self))
    }
}

impl IntoTargetInfo for Rollout {
    async fn into_info(
        self,
        _seeker: &KubeResourceSeeker<'_>,
        client: &Client,
    ) -> Result<Option<TargetInfo>, ApiError> {
        fn into_info_option(rollout: &Rollout) -> Option<TargetInfo> {
            let target_name = rollout.name()?.to_string();
            let target_namespace = rollout.namespace()?.to_string();
            Some(TargetInfo::new(
                TargetType::Rollout,
                target_name,
                target_namespace,
                vec![],
                vec![],
            ))
        }

        let result = into_info_option(&self);
        let pod_template = self.get_pod_template(client).await?;
        let containers = pod_template
            .spec
            .as_ref()
            .map(|spec| spec.containers.as_slice())
            .unwrap_or_default();
        let detected_ports = detected_ports(containers);
        let containers = container_names(containers);

        if let Some(info) = result {
            Ok(Some(TargetInfo {
                containers,
                detected_ports,
                ..info
            }))
        } else {
            Ok(None)
        }
    }
}

impl IntoTargetInfo for Job {
    async fn into_info(
        self,
        _seeker: &KubeResourceSeeker<'_>,
        _client: &Client,
    ) -> Result<Option<TargetInfo>, ApiError> {
        fn into_info_option(job: Job) -> Option<TargetInfo> {
            let target_name = job.name()?.to_string();
            let target_namespace = job.namespace()?.to_string();
            let containers = job.spec?.template.spec?.containers;
            let detected_ports = detected_ports(&containers);
            let containers = container_names(&containers);
            Some(TargetInfo::new(
                TargetType::Job,
                target_name,
                target_namespace,
                containers,
                detected_ports,
            ))
        }
        Ok(into_info_option(self))
    }
}

impl IntoTargetInfo for CronJob {
    async fn into_info(
        self,
        _seeker: &KubeResourceSeeker<'_>,
        _client: &Client,
    ) -> Result<Option<TargetInfo>, ApiError> {
        fn into_info_option(cronjob: CronJob) -> Option<TargetInfo> {
            let target_name = cronjob.name()?.to_string();
            let target_namespace = cronjob.namespace()?.to_string();
            let containers = cronjob.spec?.job_template.spec?.template.spec?.containers;
            let detected_ports = detected_ports(&containers);
            let containers = container_names(&containers);
            Some(TargetInfo::new(
                TargetType::CronJob,
                target_name,
                target_namespace,
                containers,
                detected_ports,
            ))
        }
        Ok(into_info_option(self))
    }
}

impl IntoTargetInfo for StatefulSet {
    async fn into_info(
        self,
        _seeker: &KubeResourceSeeker<'_>,
        _client: &Client,
    ) -> Result<Option<TargetInfo>, ApiError> {
        fn into_info_option(stateful_set: StatefulSet) -> Option<TargetInfo> {
            let target_name = stateful_set.name()?.to_string();
            let target_namespace = stateful_set.namespace()?.to_string();
            let containers = stateful_set.spec?.template.spec?.containers;
            let detected_ports = detected_ports(&containers);
            let containers = container_names(&containers);
            Some(TargetInfo::new(
                TargetType::StatefulSet,
                target_name,
                target_namespace,
                containers,
                detected_ports,
            ))
        }
        Ok(into_info_option(self))
    }
}

impl IntoTargetInfo for Service {
    async fn into_info(
        self,
        seeker: &KubeResourceSeeker<'_>,
        client: &Client,
    ) -> Result<Option<TargetInfo>, ApiError> {
        fn into_info_option(service: &Service) -> Option<TargetInfo> {
            let target_name = service.name()?.to_string();
            let target_namespace = service.namespace()?.to_string();
            Some(TargetInfo::new(
                TargetType::Service,
                target_name,
                target_namespace,
                vec![],
                vec![],
            ))
        }
        let result = into_info_option(&self);
        // A service selects pods matching ALL of its selector labels, so they go into one
        // comma-joined label selector; querying per label would also pick up unrelated pods
        // that share just one of the labels.
        let label_selector = self
            .spec
            .and_then(|spec| spec.selector)
            .map(|tree| {
                tree.into_iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .join(",")
            })
            .unwrap_or_default();

        let infos: Vec<TargetInfo> = if label_selector.is_empty() {
            vec![]
        } else {
            seeker
                .list_all_namespaced::<Pod>(None, Some(&label_selector))
                .filter_map(|pod_res| into_info(pod_res, seeker, client))
                .collect()
                .await
        };

        let (detected_ports, containers): (Vec<u16>, Vec<String>) = infos.into_iter().fold(
            (Vec::new(), Vec::new()),
            |(mut ports, mut containers), info| {
                ports.extend(info.detected_ports);
                containers.extend(info.containers);
                (ports, containers)
            },
        );

        if let Some(info) = result {
            Ok(Some(TargetInfo {
                containers,
                detected_ports,
                ..info
            }))
        } else {
            Ok(None)
        }
    }
}

impl IntoTargetInfo for ReplicaSet {
    async fn into_info(
        self,
        _seeker: &KubeResourceSeeker<'_>,
        _client: &Client,
    ) -> Result<Option<TargetInfo>, ApiError> {
        fn into_info_option(replica_set: ReplicaSet) -> Option<TargetInfo> {
            let target_name = replica_set.name()?.to_string();
            let target_namespace = replica_set.namespace()?.to_string();
            let containers = replica_set.spec?.template?.spec?.containers;
            let detected_ports = detected_ports(&containers);
            let containers = container_names(&containers);
            Some(TargetInfo::new(
                TargetType::ReplicaSet,
                target_name,
                target_namespace,
                containers,
                detected_ports,
            ))
        }
        Ok(into_info_option(self))
    }
}
