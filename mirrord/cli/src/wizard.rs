//! # mirrord Wizard (aka onboarding Wizard)
//!
//! The `wizard` module contains everything needed to run the `mirrord wizard` CLI command, with the
//! exception of the frontend code. This lives in a different top-level directory called
//! `wizard-frontend`. More documentation specific to the frontend exists in
//! `wizard-frontend/README.md`.
//!
//! **The Wizard operates as follows**:
//!
//! 1. Build the TypeScript frontend and compress `wizard-frontend/dist` into a Gzipped file
//! 2. Use the `include_bytes!` macro to make [`COMPRESSED_FRONTEND`] when compiled with the
//!    `wizard` feature
//! 3. When the command is run, the file is unzipped and the frontend served on localhost with axum
//!    as a Single Page App
//! 4. The wizard opens in the browser and the user can create and download a config file
//!
//! ### Working on the Backend
//!
//! Note: To compile the backend, you need to enable the `wizard` feature, for which you need the
//! compressed frontend file. This file is in the `.gitignore` and should NOT be committed.
//!
//! *Example full build and run script*:
//! ```bash
//! ## Build the frontend.
//! cd wizard-frontend/ || exit
//! npm run build
//! cd ..
//!
//! ## Zip frontend.
//! tar czf wizard-frontend.tar.gz --directory=wizard-frontend/dist .
//!
//! ## If you are not changing the frontend, the above steps do not have to be repeated.
//! ## Build mirrord w/ wizard feature.
//! cargo build --manifest-path=./Cargo.toml -p mirrord --features wizard
//!
//! ## Run the wizard command with trace level TRACE.
//! RUST_LOG=mirrord=trace ./target/debug/mirrord wizard
//! ```
//!
//! ### Backend Overview
//!
//! - Endpoints: the frontend communicates with the backend via some REST endpoints defined in
//!   `app`.
//! - [`State`]: the app state contains the tuple `(UserData, Client)` for updating the
//!   `is_returning` bool and making additional resource seeker requests respectively.
//! - [`Params`]: when listing targets, the target_type is an optional parameter (which may speed up
//!   fetching on large clusters).
//! - [`TargetInfo`] trait: all target types (except Targetless) implement
//!   [`IntoTargetInfo::into_info`] to retrieve target details from the resource definition.
//! - Errors: the wizard returns `CliError` errors, either when an io operation fails or when there
//!   is a problem seeking resources in the cluster. For the latter, errors are turned into `None`s
//!   to avoid the frontend stopping altogether.

use std::{
    io::Cursor,
    net::{Ipv4Addr, SocketAddr},
    ops::Not,
    sync::Arc,
};

use axum::{
    Router,
    extract::{Path, Query, State},
    http::header,
    routing::get,
};
use flate2::read::GzDecoder;
use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::{
    apps::v1::{Deployment, ReplicaSet, StatefulSet},
    batch::v1::{CronJob, Job},
    core::v1::{Namespace, Pod, Service},
};
use kube::{Client, client::ClientBuilder};
use mirrord_config::target::TargetType;
use mirrord_kube::{
    api::kubernetes::{create_kube_config, rollout::Rollout, seeker::KubeResourceSeeker},
    error::KubeApiError,
};
use mirrord_progress::Progress;
use serde::{Deserialize, Serialize};
use tar::Archive;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tower::ServiceBuilder;
use tower_http::{
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};

use crate::{
    config::WizardArgs,
    error::{CliError, CliResult},
    user_data::UserData,
};

/// The frontend `dist` dir, compressed (as bytes). The CI runs this step automatically, so you only
/// need to do it manually if making changes to the wizard.
const COMPRESSED_FRONTEND: &[u8] = include_bytes!("../../../wizard-frontend.tar.gz");

/// The entrypoint for the `wizard` command. Unzips the frontend and serves it using axum, along
/// with internal API endpoints.
pub async fn wizard_command<P>(
    user_data: UserData,
    args: WizardArgs,
    parent_progress: &mut P,
) -> CliResult<()>
where
    P: Progress,
{
    let mut progress = parent_progress.subtask("launching wizard");
    let is_returning_string = user_data.is_returning_wizard().to_string();

    // client for fetching targets from the cluster when detecting exposed ports
    let client = create_kube_config(
        args.accept_invalid_certificates,
        args.kubeconfig,
        args.context,
    )
    .await
    .and_then(|config| Ok(ClientBuilder::try_from(config.clone())?.build()))
    .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateKubeApiFailed))?;

    // unpack the frontend into `TempDir`
    let tmp_dir = TempDir::new()?;
    let temp_dir_path = tmp_dir.path();

    let tar_gz = Cursor::new(COMPRESSED_FRONTEND);
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);
    archive.unpack(temp_dir_path)?;

    let serve_client = {
        let index_service = ServiceBuilder::new()
            .layer(SetResponseHeaderLayer::overriding(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-cache, no-store, must-revalidate"),
            ))
            .layer(SetResponseHeaderLayer::overriding(
                header::PRAGMA,
                header::HeaderValue::from_static("no-cache"),
            ))
            .layer(SetResponseHeaderLayer::overriding(
                header::EXPIRES,
                header::HeaderValue::from_static("0"),
            ))
            .service(ServeFile::new(temp_dir_path.join("index.html")));

        ServiceBuilder::new()
            .layer(SetResponseHeaderLayer::if_not_present(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("public, max-age=1209600"),
            ))
            .service(
                ServeDir::new(temp_dir_path)
                    .fallback(index_service)
                    .append_index_html_on_directories(false),
            )
    };

    // prepare temp dir as SPA with endpoints behind `/api` path
    let app = Router::new()
        .layer(TraceLayer::new_for_http())
        .fallback_service(serve_client)
        .route(
            "/api/v1/is-returning",
            get(|| async { is_returning_string }),
        )
        .route("/api/v1/cluster-details", get(cluster_details))
        .route("/api/v1/namespace/{namespace}/targets", get(list_targets))
        .with_state(Arc::new(Mutex::new((user_data, client))));
    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 3000);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::debug!("listening on {}", listener.local_addr()?);
    progress.success(None);

    parent_progress
        .subtask("<|:) Greetings, traveler!")
        .success(None);

    // open browser window
    let url = "http://localhost:3000/";
    match opener::open(url) {
        Ok(()) => {}
        Err(error) => {
            tracing::trace!(?error, "failed to open browser");
            parent_progress
                .subtask("Couldn't open the browser automatically")
                .failure(None);
        }
    }
    parent_progress
        .subtask(format!("The wizard is available at: {url}").as_str())
        .success(None);

    parent_progress.success(None);
    axum::serve(listener, app.layer(TraceLayer::new_for_http()))
        .await
        .expect("Infallible operation should never fail");

    Ok(())
}

/// The response returned from the `/api/v1/cluster-details` endpoint with namespaces and
/// target_types that mirrord can target in the user's cluster.
#[derive(Debug, Serialize)]
struct ClusterDetails {
    namespaces: Vec<String>,
    target_types: Vec<TargetType>,
}

/// Represents the relevant information for each available target returned by the
/// `/api/v1/namespace/{namespace}/targets` endpoint. `detected_ports` are used in port
/// configuration in the wizard's Network tab to offer ports that the user is most likely to be
/// interested in, according to the ports exposed by that target's Pod or Pods
#[derive(Debug, Serialize)]
struct TargetInfo {
    target_path: String,
    target_namespace: String,
    detected_ports: Vec<u16>,
}

impl TargetInfo {
    /// Create a new [`Self`], without having to construct the `target_path` manually
    fn new(
        target_type: TargetType,
        target_name: String,
        target_namespace: String,
        detected_ports: Vec<u16>,
    ) -> Self {
        Self {
            target_path: format!("{target_type}/{target_name}"),
            target_namespace,
            detected_ports,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Params {
    target_type: Option<TargetType>,
}

/// Called by the `/api/v1/cluster-details` endpoint, returning cluster details that are shown while
/// the user selects a target.
#[tracing::instrument(level = tracing::Level::TRACE, skip_all, ret)]
async fn cluster_details(State(arc): State<Arc<Mutex<(UserData, Client)>>>) -> CliResult<String> {
    let mut user_guard = arc.lock().await;
    // consider the user a returning user in future runs
    if user_guard.0.is_returning_wizard().not() {
        // ignore failures to update
        let _ = user_guard.0.update_is_returning_wizard().await;
    }

    // return available namespaces
    let client = user_guard.1.clone();
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
        .map_err(KubeApiError::KubeError)
        .map_err(|error| CliError::friendlier_error_or_else(error, CliError::ListTargetsFailed))?;

    let res = ClusterDetails {
        namespaces,
        target_types: TargetType::all()
            .filter(|&t| t != TargetType::Targetless)
            .collect(),
    };
    Ok(serde_json::to_string(&res)?)
}

/// Called by the `/api/v1/namespace/{namespace}/targets` endpoint, returning details for individual
/// targets. Targets can only be listed in a single namespace, and may optionally only be listed for
/// a single [`TargetType`].
#[tracing::instrument(level = tracing::Level::TRACE, skip_all, ret)]
async fn list_targets(
    State(arc): State<Arc<Mutex<(UserData, Client)>>>,
    Query(query_params): Query<Params>,
    Path(namespace): Path<String>,
) -> CliResult<String> {
    let user_guard = arc.lock().await;
    let client = user_guard.1.clone();
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

    Ok(serde_json::to_string(&targets)?)
}

/// A helper function that bridges the gap between the types in
/// [`list_all_namespaced`][seeker::KubeResourceSeeker::list_all_namespaced] and
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
}

/// Implementable by resources that can be targeted by mirrord and can produce a [`TargetInfo`].
trait IntoTargetInfo {
    /// Construct an instance of [`TargetInfo`] from a valid target Resource. Fetches further
    /// resources in some cases, to detect ports on the resource's pods.
    ///
    /// If fetching further resources fails, return a `CliError`. If there are any empty fields in
    /// the Resource spec, return `None`.
    fn into_info(
        self,
        seeker: &KubeResourceSeeker,
        client: &Client,
    ) -> impl Future<Output = Result<Option<TargetInfo>, CliError>>;
}

impl IntoTargetInfo for Pod {
    async fn into_info(
        self,
        _seeker: &KubeResourceSeeker<'_>,
        _client: &Client,
    ) -> Result<Option<TargetInfo>, CliError> {
        fn into_info_option(pod: Pod) -> Option<TargetInfo> {
            let target_name = pod.metadata.name.clone()?;
            let target_namespace = pod.metadata.namespace.clone()?;
            let detected_ports = pod
                .spec?
                .containers
                .iter()
                .flat_map(|container| container.ports.clone().unwrap_or_default())
                .map(|port| port.container_port.unsigned_abs() as u16)
                .collect();
            Some(TargetInfo::new(
                TargetType::Pod,
                target_name,
                target_namespace,
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
    ) -> Result<Option<TargetInfo>, CliError> {
        fn into_info_option(deployment: Deployment) -> Option<TargetInfo> {
            let target_name = deployment.metadata.name.clone()?;
            let target_namespace = deployment.metadata.namespace.clone()?;
            let detected_ports = deployment
                .spec?
                .template
                .spec?
                .containers
                .iter()
                .flat_map(|container| container.ports.clone().unwrap_or_default())
                .map(|port| port.container_port.unsigned_abs() as u16)
                .collect();
            Some(TargetInfo::new(
                TargetType::Deployment,
                target_name,
                target_namespace,
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
    ) -> Result<Option<TargetInfo>, CliError> {
        fn into_info_option(job: &Rollout) -> Option<TargetInfo> {
            let target_name = job.metadata.name.clone()?;
            let target_namespace = job.metadata.namespace.clone()?;
            Some(TargetInfo::new(
                TargetType::Job,
                target_name,
                target_namespace,
                vec![],
            ))
        }

        let result = into_info_option(&self);
        let detected_ports = self
            .get_pod_template(client)
            .await
            .map_err(CliError::WizardTargetError)?
            .spec
            .as_ref()
            .map(|spec| spec.containers.clone())
            .unwrap_or_default()
            .iter()
            .flat_map(|container| container.ports.clone().unwrap_or_default())
            .map(|port| port.container_port.unsigned_abs() as u16)
            .collect();

        if let Some(info) = result {
            Ok(Some(TargetInfo {
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
    ) -> Result<Option<TargetInfo>, CliError> {
        fn into_info_option(job: Job) -> Option<TargetInfo> {
            let target_name = job.metadata.name.clone()?;
            let target_namespace = job.metadata.namespace.clone()?;
            let detected_ports = job
                .spec?
                .template
                .spec?
                .containers
                .iter()
                .flat_map(|container| container.ports.clone().unwrap_or_default())
                .map(|port| port.container_port.unsigned_abs() as u16)
                .collect();
            Some(TargetInfo::new(
                TargetType::Job,
                target_name,
                target_namespace,
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
    ) -> Result<Option<TargetInfo>, CliError> {
        fn into_info_option(cronjob: CronJob) -> Option<TargetInfo> {
            let target_name = cronjob.metadata.name.clone()?;
            let target_namespace = cronjob.metadata.namespace.clone()?;
            let detected_ports = cronjob
                .spec?
                .job_template
                .spec?
                .template
                .spec?
                .containers
                .iter()
                .flat_map(|container| container.ports.clone().unwrap_or_default())
                .map(|port| port.container_port.unsigned_abs() as u16)
                .collect();
            Some(TargetInfo::new(
                TargetType::CronJob,
                target_name,
                target_namespace,
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
    ) -> Result<Option<TargetInfo>, CliError> {
        fn into_info_option(stateful_set: StatefulSet) -> Option<TargetInfo> {
            let target_name = stateful_set.metadata.name.clone()?;
            let target_namespace = stateful_set.metadata.namespace.clone()?;
            let detected_ports = stateful_set
                .spec?
                .template
                .spec?
                .containers
                .iter()
                .flat_map(|container| container.ports.clone().unwrap_or_default())
                .map(|port| port.container_port.unsigned_abs() as u16)
                .collect();
            Some(TargetInfo::new(
                TargetType::StatefulSet,
                target_name,
                target_namespace,
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
    ) -> Result<Option<TargetInfo>, CliError> {
        fn into_info_option(service: &Service) -> Option<TargetInfo> {
            let target_name = service.metadata.name.clone()?;
            let target_namespace = service.metadata.namespace.clone()?;
            Some(TargetInfo::new(
                TargetType::Service,
                target_name,
                target_namespace,
                vec![],
            ))
        }
        let result = into_info_option(&self);
        let selector: Vec<_> = self
            .spec
            .and_then(|spec| spec.selector)
            .map(|tree| tree.into_iter().collect())
            .unwrap_or_default();

        let mut infos: Vec<TargetInfo> = vec![];
        for (key, value) in selector {
            let label_selector = format!("{key}={value}");
            infos.extend(
                seeker
                    .list_all_namespaced::<Pod>(None, Some(&label_selector))
                    .filter_map(|pod_res| into_info(pod_res, seeker, client))
                    .collect::<Vec<_>>()
                    .await,
            );
        }

        let detected_ports: Vec<u16> = infos
            .into_iter()
            .flat_map(|info| info.detected_ports)
            .collect();

        if let Some(info) = result {
            Ok(Some(TargetInfo {
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
    ) -> Result<Option<TargetInfo>, CliError> {
        fn into_info_option(replica_set: ReplicaSet) -> Option<TargetInfo> {
            let target_name = replica_set.metadata.name.clone()?;
            let target_namespace = replica_set.metadata.namespace.clone()?;
            let detected_ports = replica_set
                .spec?
                .template?
                .spec?
                .containers
                .iter()
                .flat_map(|container| container.ports.clone().unwrap_or_default())
                .map(|port| port.container_port.unsigned_abs() as u16)
                .collect();
            Some(TargetInfo::new(
                TargetType::ReplicaSet,
                target_name,
                target_namespace,
                detected_ports,
            ))
        }
        Ok(into_info_option(self))
    }
}
