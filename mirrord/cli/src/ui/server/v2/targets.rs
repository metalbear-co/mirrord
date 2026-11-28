//! Cluster **target** enumeration, served under `/api/v2/kube/{targets,target-types}`. The config
//! wizard lists targets through `/api/v1/up/workloads` (see `ui::wizard`).

use axum::extract::Query;
use futures::StreamExt;
use itertools::Itertools;
use k8s_openapi::api::{
    apps::v1::{Deployment, ReplicaSet, StatefulSet},
    batch::v1::{CronJob, Job},
    core::v1::{Container, Pod, Service},
};
use kube::{Api, Client, api::ListParams, runtime::reflector::Lookup};
use mirrord_config::target::{Target, TargetDisplay, TargetType};
use mirrord_kube::api::kubernetes::{rollout::Rollout, seeker::KubeResourceSeeker};
use mirrord_operator::crd::TargetCrd;
use serde::{Deserialize, Serialize};

use crate::ui::{
    error::ApiError,
    server::{client_for_context, daemon_login},
};

/// ### Response for `GET /api/v2/kube/target-types`
///
/// The resource types mirrord can target. This is a static list (it doesn't touch the cluster); it
/// lives here next to the target enumeration it pairs with.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TargetTypesResponse {
    target_types: Vec<TargetType>,
}

/// Represents the relevant information for each available target returned by the
/// `GET /api/v2/kube/targets` endpoint.
#[derive(Debug, Serialize)]
pub(crate) struct TargetInfo {
    /// Like `deployment/api`.
    target_path: String,
    target_namespace: String,
    /// Of the target's pod template.
    containers: Vec<String>,
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
pub(crate) struct TargetsQuery {
    target_type: Option<TargetType>,
    /// Kube context to query. When absent, the kubeconfig's current context is used. The old
    /// standalone `mirrord wizard` took this as a CLI flag; the shared server outlives any single
    /// invocation, so the frontend selects it per request instead.
    context: Option<String>,
    /// Namespace to list targets in. When absent, the `default` namespace is used.
    namespace: Option<String>,
}

/// Lists the resource types mirrord can target. Static (no cluster access); served at
/// `GET /api/v2/kube/target-types`.
pub(crate) async fn list_target_types() -> axum::Json<TargetTypesResponse> {
    axum::Json(TargetTypesResponse {
        target_types: TargetType::all()
            .filter(|&t| t != TargetType::Targetless)
            .collect(),
    })
}

/// Returns details for individual targets. Targets can only be listed in a single namespace, and
/// may optionally be restricted to a single [`TargetType`]. Served at `GET /api/v2/kube/targets`.
#[tracing::instrument(level = tracing::Level::TRACE, skip_all, ret)]
pub(crate) async fn list_targets(
    Query(query_params): Query<TargetsQuery>,
) -> Result<axum::Json<Vec<TargetInfo>>, ApiError> {
    let namespace = query_params.namespace.as_deref().unwrap_or("default");
    let client = client_for_context(query_params.context.as_deref()).await?;

    // Only the operator's own API answers an operator login, so it lists what the operator sees,
    // as `mirrord ls` does under a login.
    if daemon_login().await?.is_some() {
        let listed = Api::<TargetCrd>::namespaced(client, namespace)
            .list(&ListParams::default())
            .await
            .map_err(ApiError::KubeApi)?;
        let targets = listed
            .into_iter()
            .filter_map(|crd| crd.spec.target.as_known().ok().cloned());
        return Ok(axum::Json(infos_from_operator_targets(
            targets,
            namespace,
            query_params.target_type,
        )));
    }

    let seeker = KubeResourceSeeker {
        client: &client,
        namespace,
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
            // frontend does not yet support targetless, label and serverless
            TargetType::Targetless | TargetType::Label | TargetType::Serverless => vec![],
        });
    }

    Ok(axum::Json(targets))
}

/// One entry per workload from the targets the operator lists, which name one container each, of
/// `target_type` when given. The operator reports no ports, so `detected_ports` stays empty under
/// a login.
fn infos_from_operator_targets(
    targets: impl IntoIterator<Item = Target>,
    namespace: &str,
    target_type: Option<TargetType>,
) -> Vec<TargetInfo> {
    let mut infos: Vec<TargetInfo> = Vec::new();
    for target in targets {
        let kind = TargetType::from(&target);
        let unwanted = target_type.is_some_and(|wanted| wanted != kind);
        if unwanted
            || matches!(
                kind,
                TargetType::Targetless | TargetType::Label | TargetType::Serverless
            )
        {
            continue;
        }
        let path = format!("{kind}/{}", target.name());
        let container = target.container().cloned();
        match infos.iter_mut().find(|info| info.target_path == path) {
            Some(info) => info.containers.extend(container),
            None => infos.push(TargetInfo::new(
                kind,
                target.name().to_owned(),
                namespace.to_owned(),
                container.into_iter().collect(),
                Vec::new(),
            )),
        }
    }
    infos
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

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod operator_target_tests {
    use std::str::FromStr;

    use super::*;

    fn target(path: &str) -> Target {
        Target::from_str(path).expect("valid test target")
    }

    /// The operator lists a workload once per container; the page gets it once, with every
    /// container, and a type filter keeps only that type.
    #[test]
    fn one_entry_per_workload_with_its_containers() {
        let targets = [
            target("deployment/api/container/app"),
            target("deployment/api/container/sidecar"),
            target("pod/api-0/container/app"),
        ];

        let all = infos_from_operator_targets(targets.clone(), "team", None);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].target_path, "deployment/api");
        assert_eq!(all[0].containers, ["app", "sidecar"]);
        assert_eq!(all[0].target_namespace, "team");
        assert!(all[0].detected_ports.is_empty());

        let pods = infos_from_operator_targets(targets, "team", Some(TargetType::Pod));
        assert_eq!(pods.len(), 1);
        assert_eq!(pods[0].target_path, "pod/api-0");
    }
}
