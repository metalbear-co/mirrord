use std::{io::Cursor, net::SocketAddr, sync::Arc};

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
use serde::{Deserialize, Serialize};
use tar::Archive;
use tempfile::TempDir;
use tokio::{process::Command, sync::Mutex};
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

const COMPRESSED_FRONTEND: &[u8] = include_bytes!("../../../wizard-frontend.tar.gz");
#[cfg(target_os = "linux")]
fn get_open_command() -> Command {
    let mut command = Command::new("gio");
    command.arg("open");
    command
}

#[cfg(target_os = "macos")]
fn get_open_command() -> Command {
    Command::new("open")
}

/// WARNING: untested on `target_os = "windows"`, but if this fails the URL will get printed to the
/// terminal as a fallback.
#[cfg(target_os = "windows")]
fn get_open_command() -> Command {
    Command::new("cmd.exe").arg("/C").arg("start").arg("")
}

pub async fn wizard_command(user_data: UserData, args: WizardArgs) -> CliResult<()> {
    println!("<|:) Greetings, traveler!");
    println!("Opening the Wizard in the browser...");

    let is_returning_string = user_data.is_returning_wizard().to_string();

    // client
    let client = create_kube_config(
        args.accept_invalid_certificates,
        args.kubeconfig,
        args.context,
    )
    .await
    .and_then(|config| Ok(ClientBuilder::try_from(config.clone())?.build()))
    .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateKubeApiFailed))?;

    // unpack frontend into dir
    let tmp_dir = TempDir::new().unwrap(); // FIXME: remove unwraps
    let temp_dir_path = tmp_dir.path();

    let tar_gz = Cursor::new(COMPRESSED_FRONTEND);
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);
    archive.unpack(temp_dir_path).unwrap(); // FIXME: remove unwraps

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
        .fallback_service(serve_client)
        .route(
            "/api/v1/is-returning",
            get(|| async { is_returning_string }),
        )
        .route("/api/v1/cluster-details", get(cluster_details))
        .route("/api/v1/namespace/{namespace}/targets", get(list_targets))
        .with_state(Arc::new(Mutex::new((user_data, client))));
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap(); // FIXME: remove unwraps
    tracing::debug!("listening on {}", listener.local_addr().unwrap()); // FIXME: remove unwraps

    // open browser window
    let url = "http://localhost:3000/";
    match crate::wizard::get_open_command().arg(&url).output().await {
        Ok(output) if output.status.success() => {}
        other => {
            tracing::trace!(?other, "failed to open browser");
            println!(
                "To open the mirrord wizard, visit:\n\n\
             {url}"
            );
        }
    }

    axum::serve(listener, app.layer(TraceLayer::new_for_http()))
        .await
        .unwrap(); // TODO: <|:)
    Ok(())
}

#[derive(Debug, Serialize)]
struct ClusterDetails {
    namespaces: Vec<String>,
    target_types: Vec<TargetType>,
}

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

async fn cluster_details(State(arc): State<Arc<Mutex<(UserData, Client)>>>) -> CliResult<String> {
    let mut user_guard = arc.lock().await;
    // consider the user a returning user in future runs
    if !user_guard.0.is_returning_wizard() {
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

async fn list_targets(
    State(arc): State<Arc<Mutex<(UserData, Client)>>>,
    Query(query_params): Query<Params>,
    Path(namespace): Path<String>,
) -> CliResult<String> {
    // return target info using mirrord ls
    let user_guard = arc.lock().await;
    let client = user_guard.1.clone();
    let seeker = KubeResourceSeeker {
        client: &client,
        namespace: &namespace,
        copy_target: true,
    };

    // return target according to target type param, otherwise fetch all types (sans Targetless)
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
                    .list_all_namespaced::<Deployment>(None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::Pod => {
                seeker
                    .list_all_namespaced::<Pod>(None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::Rollout => {
                seeker
                    .list_all_namespaced::<Rollout>(None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::Job => {
                seeker
                    .list_all_namespaced::<Job>(None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::CronJob => {
                seeker
                    .list_all_namespaced::<CronJob>(None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::StatefulSet => {
                seeker
                    .list_all_namespaced::<StatefulSet>(None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::Service => {
                seeker
                    .list_all_namespaced::<Service>(None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::ReplicaSet => {
                seeker
                    .list_all_namespaced::<ReplicaSet>(None)
                    .filter_map(|x| into_info(x, &seeker, &client))
                    .collect::<Vec<_>>()
                    .await
            }
            TargetType::Targetless => vec![], // the frontend does not yet support targetless
        });
    }

    Ok(serde_json::to_string(&targets)?)
}

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

trait IntoTargetInfo {
    /// Construct an instance of [`TargetInfo`] from a valid target Resource. If fetching further
    /// resources fails, return a `CliError`. If there are any empty fields in the Resource spec,
    /// return `None`.
    fn into_info(
        self,
        seeker: &KubeResourceSeeker,
        client: &Client,
    ) -> impl Future<Output=Result<Option<TargetInfo>, CliError>>;
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
                .map(|port| port.container_port.abs() as u16)
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
                .map(|port| port.container_port.abs() as u16)
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
            .map(|spec| spec.containers.clone()).unwrap_or_default()
            .iter()
            .flat_map(|container| container.ports.clone().unwrap_or_default())
            .map(|port| port.container_port.abs() as u16)
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
                .map(|port| port.container_port.abs() as u16)
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
                .map(|port| port.container_port.abs() as u16)
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
                .map(|port| port.container_port.abs() as u16)
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
            .and_then(|tree| Some(tree.into_iter().collect()))
            .unwrap_or_default();

        let mut infos: Vec<TargetInfo> = vec![];
        for (key, value) in selector {
            let key_value = format!("{key}={value}");
            infos.extend(
                seeker
                    .list_all_namespaced::<Pod>(Some(&key_value))
                    .filter_map(|x| into_info(x, &seeker, &client))
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
                .map(|port| port.container_port.abs() as u16)
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
