use std::str::FromStr;

use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::{
    apps::v1::Deployment,
    batch::v1::Job,
    core::v1::{EphemeralContainer, Pod},
};
use kube::{
    api::{Api, ListParams, LogParams, Portforwarder, PostParams},
    runtime::{watcher, WatchStreamExt},
    Client, Config,
};
use rand::distributions::{Alphanumeric, DistString};
use serde_json::{json, to_vec};
use tokio::pin;
use tracing::{debug, info, warn};

use crate::{
    config::LayerConfig,
    error::{LayerError, Result},
};

struct EnvVarGuard {
    library: String,
}

impl EnvVarGuard {
    #[cfg(target_os = "linux")]
    const ENV_VAR: &str = "LD_PRELOAD";
    #[cfg(target_os = "macos")]
    const ENV_VAR: &str = "DYLD_INSERT_LIBRARIES";

    fn new() -> Self {
        let library = std::env::var(EnvVarGuard::ENV_VAR).unwrap_or_default();
        std::env::remove_var(EnvVarGuard::ENV_VAR);
        Self { library }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        std::env::set_var(EnvVarGuard::ENV_VAR, &self.library);
    }
}

struct RuntimeData {
    container_id: String,
    container_runtime: String,
    node_name: String,
    socket_path: String,
}

impl RuntimeData {
    async fn from_k8s(client: Client, target: &str, pod_namespace: &str) -> Result<Self> {
        let target = target.parse::<Target>()?;

        let pods_api: Api<Pod> = Api::namespaced(client.clone(), pod_namespace);
        let deployment_api: Api<Deployment> = Api::namespaced(client.clone(), pod_namespace);

        // TODO: figure a way to pass the Api in a generic way, don't want to pass two different
        // Apis, but match on the Target enum and pass accordingly
        let container_info = target
            .container_info(&pods_api, &deployment_api, pod_namespace)
            .await;

        let container_info = container_info
            .as_ref()
            .unwrap()
            .split("://")
            .collect::<Vec<&str>>();

        let (container_runtime, socket_path) = match container_info.first() {
            Some(&"docker") => ("docker", "/var/run/docker.sock"),
            Some(&"containerd") => ("containerd", "/run/containerd/containerd.sock"),
            _ => panic!("unsupported container runtime"),
        };

        let container_id = container_info.last().unwrap();

        Ok(RuntimeData {
            container_id: container_id.to_string(),
            container_runtime: container_runtime.to_string(),
            node_name: target
                .node_name(pods_api)
                .await
                .as_ref()
                .unwrap()
                .to_string(),
            socket_path: socket_path.to_string(),
        })
    }
}

pub(crate) async fn create_agent(
    config: LayerConfig,
    connection_port: u16,
) -> Result<Portforwarder> {
    let _guard = EnvVarGuard::new();
    let LayerConfig {
        agent_image,
        agent_namespace,
        target,
        impersonated_pod_namespace,
        ephemeral_container,
        accept_invalid_certificates,
        ..
    } = config.clone();

    let client = if accept_invalid_certificates {
        let mut config = Config::infer().await?;
        config.accept_invalid_certs = true;
        warn!("Accepting invalid certificates");
        Client::try_from(config).map_err(LayerError::KubeError)?
    } else {
        Client::try_default().await.map_err(LayerError::KubeError)?
    };

    let pods_api: Api<Pod> = Api::namespaced(
        client.clone(),
        agent_namespace
            .as_ref()
            .unwrap_or(&impersonated_pod_namespace),
    );

    let agent_image = agent_image.unwrap_or_else(|| {
        concat!("ghcr.io/metalbear-co/mirrord:", env!("CARGO_PKG_VERSION")).to_string()
    });

    let pod_name = if ephemeral_container {
        let pod_target = target.parse::<PodData>().unwrap();

        create_ephemeral_container_agent(
            &config,
            pod_target,
            agent_image,
            &pods_api,
            connection_port,
        )
        .await?
    } else {
        let runtime_data =
            RuntimeData::from_k8s(client.clone(), &target, &impersonated_pod_namespace)
                .await
                .map_err(LayerError::from)?;

        let jobs_api: Api<Job> = Api::namespaced(
            client.clone(),
            agent_namespace
                .as_ref()
                .unwrap_or(&impersonated_pod_namespace),
        );

        create_job_pod_agent(
            &config,
            agent_image,
            &pods_api,
            runtime_data,
            &jobs_api,
            connection_port,
        )
        .await?
    };
    pods_api
        .portforward(&pod_name, &[connection_port])
        .await
        .map_err(LayerError::KubeError)
}

fn get_agent_name() -> String {
    let agent_name = format!(
        "mirrord-agent-{}",
        Alphanumeric
            .sample_string(&mut rand::thread_rng(), 10)
            .to_lowercase()
    );
    agent_name
}

fn is_ephemeral_container_running(pod: Pod, container_name: &String) -> bool {
    debug!("pod status: {:?}", &pod.status);
    pod.status
        .and_then(|status| {
            status
                .ephemeral_container_statuses
                .and_then(|container_statuses| {
                    container_statuses
                        .iter()
                        .find(|&status| &status.name == container_name)
                        .and_then(|status| {
                            status.state.as_ref().map(|state| state.running.is_some())
                        })
                })
        })
        .unwrap_or(false)
}

async fn wait_for_agent_startup(
    pods_api: &Api<Pod>,
    pod_name: &str,
    container_name: String,
) -> Result<()> {
    let mut logs = pods_api
        .log_stream(
            pod_name,
            &LogParams {
                follow: true,
                container: Some(container_name),
                ..LogParams::default()
            },
        )
        .await?;

    while let Some(line) = logs.try_next().await? {
        let line = String::from_utf8_lossy(&line);
        if line.contains("agent ready") {
            break;
        }
    }
    Ok(())
}

async fn create_ephemeral_container_agent(
    config: &LayerConfig,
    pod_target: PodData,
    agent_image: String,
    pods_api: &Api<Pod>,
    connection_port: u16,
) -> Result<String> {
    warn!("Ephemeral Containers is an experimental feature
              >> Refer https://kubernetes.io/docs/concepts/workloads/pods/ephemeral-containers/ for more info");

    let mirrord_agent_name = get_agent_name();

    let mut agent_command_line = vec![
        "./mirrord-agent".to_string(),
        "-l".to_string(),
        connection_port.to_string(),
        "-e".to_string(),
    ];
    if let Some(timeout) = config.agent_communication_timeout {
        agent_command_line.push("-t".to_string());
        agent_command_line.push(timeout.to_string());
    }

    let ephemeral_container: EphemeralContainer = serde_json::from_value(json!({
        "name": mirrord_agent_name,
        "image": agent_image,
        "securityContext": {
            "capabilities": {
                "add": ["NET_RAW", "NET_ADMIN"],
            },
            "privileged": true,
        },
        "imagePullPolicy": config.image_pull_policy,
        "targetContainerName": pod_target.container_name,
        "env": [{"name": "RUST_LOG", "value": config.agent_rust_log}],
        "command": agent_command_line,
    }))?;
    debug!("Requesting ephemeral_containers_subresource");

    let mut ephemeral_containers_subresource = pods_api
        .get_subresource("ephemeralcontainers", &pod_target.pod_name)
        .await
        .map_err(LayerError::KubeError)?;

    let mut spec = ephemeral_containers_subresource
        .spec
        .as_mut()
        .ok_or_else(|| LayerError::PodSpecNotFound(pod_target.pod_name.clone()))?;

    spec.ephemeral_containers = match spec.ephemeral_containers.clone() {
        Some(mut ephemeral_containers) => {
            ephemeral_containers.push(ephemeral_container);
            Some(ephemeral_containers)
        }
        None => Some(vec![ephemeral_container]),
    };

    pods_api
        .replace_subresource(
            "ephemeralcontainers",
            &pod_target.pod_name,
            &PostParams::default(),
            to_vec(&ephemeral_containers_subresource).map_err(LayerError::from)?,
        )
        .await
        .map_err(LayerError::KubeError)?;

    let params = ListParams::default()
        .fields(&format!("metadata.name={}", &pod_target.pod_name))
        .timeout(60);

    let stream = watcher(pods_api.clone(), params).applied_objects();
    pin!(stream);

    while let Some(Ok(pod)) = stream.next().await {
        if is_ephemeral_container_running(pod, &mirrord_agent_name) {
            debug!("container ready");
            break;
        } else {
            debug!("container not ready yet");
        }
    }

    wait_for_agent_startup(pods_api, &pod_target.pod_name, mirrord_agent_name).await?;

    debug!("container is ready");
    Ok(pod_target.pod_name.clone())
}

async fn create_job_pod_agent(
    config: &LayerConfig,
    agent_image: String,
    pods_api: &Api<Pod>,
    runtime_data: RuntimeData,
    job_api: &Api<Job>,
    connection_port: u16,
) -> Result<String> {
    let mirrord_agent_job_name = get_agent_name();

    let mut agent_command_line = vec![
        "./mirrord-agent".to_string(),
        "--container-id".to_string(),
        runtime_data.container_id,
        "--container-runtime".to_string(),
        runtime_data.container_runtime,
        "-l".to_string(),
        connection_port.to_string(),
    ];
    if let Some(timeout) = config.agent_communication_timeout {
        agent_command_line.push("-t".to_string());
        agent_command_line.push(timeout.to_string());
    }

    let agent_pod: Job =
        serde_json::from_value(json!({ // Only Jobs support self deletion after completion
                "metadata": {
                    "name": mirrord_agent_job_name,
                    "labels": {
                        "app": "mirrord"
                    }
                },
                "spec": {
                "ttlSecondsAfterFinished": config.agent_ttl,

                    "template": {
                "spec": {
                    "hostPID": true,
                    "nodeName": runtime_data.node_name,
                    "restartPolicy": "Never",
                    "volumes": [
                        {
                            "name": "sockpath",
                            "hostPath": {
                                "path": runtime_data.socket_path
                            }
                        }
                    ],
                    "containers": [
                        {
                            "name": "mirrord-agent",
                            "image": agent_image,
                            "imagePullPolicy": config.image_pull_policy,
                            "securityContext": {
                                "privileged": true,
                            },
                            "volumeMounts": [
                                {
                                    "mountPath": runtime_data.socket_path,
                                    "name": "sockpath"
                                }
                            ],
                            "command": agent_command_line,
                            "env": [{"name": "RUST_LOG", "value": config.agent_rust_log}],
                        }
                    ]
                }
            }
        }
            }
        ))?;
    job_api
        .create(&PostParams::default(), &agent_pod)
        .await
        .map_err(LayerError::KubeError)?;

    let params = ListParams::default()
        .labels(&format!("job-name={}", mirrord_agent_job_name))
        .timeout(60);

    let stream = watcher(pods_api.clone(), params).applied_objects();
    pin!(stream);

    while let Some(Ok(pod)) = stream.next().await {
        if let Some(status) = &pod.status && let Some(phase) = &status.phase {
                    debug!("Pod Phase = {phase:?}");
                if phase == "Running" {
                    break;
                }
            }
    }

    let pods = pods_api
        .list(&ListParams::default().labels(&format!("job-name={}", mirrord_agent_job_name)))
        .await
        .map_err(LayerError::KubeError)?;

    let pod_name = pods
        .items
        .first()
        .and_then(|pod| pod.metadata.name.clone())
        .ok_or(LayerError::PodNotFound(mirrord_agent_job_name))?;

    wait_for_agent_startup(pods_api, &pod_name, "mirrord-agent".to_string()).await?;
    Ok(pod_name)
}

#[derive(Debug)]
pub(crate) struct DeploymentData {
    deployment: String,
}

impl DeploymentData {
    pub async fn container_id(
        &self,
        deployment_api: &Api<Deployment>,
        pod_api: &Api<Pod>,
        pod_namespace: &str,
    ) -> Option<String> {
        let deployment = deployment_api.get(&self.deployment).await.unwrap();
        let pod_name = deployment
            .spec
            .unwrap()
            .template
            .metadata
            .unwrap()
            .name
            .unwrap();
        let pod = PodData {
            pod_name,
            container_name: None,
        };
        pod.container_id(pod_api, pod_namespace).await
    }
}

#[derive(Debug)]
pub(crate) enum Target {
    Deployment(DeploymentData),
    Pod(PodData),
}

impl Target {
    pub async fn container_info(
        &self,
        pod_api: &Api<Pod>,
        deployment_api: &Api<Deployment>,
        pod_namespace: &str,
    ) -> Option<String> {
        match self {
            Target::Pod(pod) => pod.container_id(pod_api, pod_namespace).await,
            Target::Deployment(deployment) => {
                deployment
                    .container_id(deployment_api, pod_api, pod_namespace)
                    .await
            }
        }
    }

    pub async fn node_name(&self, pods_api: Api<Pod>) -> Option<String> {
        match self {
            Target::Pod(PodData {
                pod_name,
                container_name: _,
            }) => {
                let pod = pods_api.get(pod_name).await.unwrap();
                pod.spec.unwrap().node_name
            }
            Target::Deployment(_) => None,
        }
    }
}

impl FromStr for DeploymentData {
    type Err = LayerError;

    fn from_str(s: &str) -> Result<Self> {
        let split = s.split('/').collect::<Vec<&str>>();
        match split.first() {
            Some(&"deployment") => Ok(DeploymentData {
                deployment: split[1].to_string(),
            }),
            _ => Err(LayerError::InvalidTarget(format!(
                "Given target: {:?} is not a deployment.",
                s
            ))),
        }
    }
}

#[derive(Debug)]
pub(crate) struct PodData {
    pub pod_name: String,
    pub container_name: Option<String>,
}

impl FromStr for PodData {
    type Err = LayerError;

    fn from_str(s: &str) -> Result<Self> {
        let split = s.split('/').collect::<Vec<&str>>();
        match split.first() {
            Some(&"pod") if split.len() == 2 => Ok(PodData {
                pod_name: split[1].to_string(),
                container_name: None,
            }),
            Some(&"pod") if split.len() == 4 && split[2] == "container" => Ok(PodData {
                pod_name: split[1].to_string(),
                container_name: Some(split[3].to_string()),
            }),
            _ => Err(LayerError::InvalidTarget(format!(
                "Given target: {:?} is not a pod.",
                s
            ))),
        }
    }
}

impl PodData {
    pub async fn container_id(&self, pods_api: &Api<Pod>, pod_namespace: &str) -> Option<String> {
        let pod = pods_api.get(&self.pod_name).await.unwrap();
        let container_statuses = &pod.status.unwrap().container_statuses.unwrap();
        let container_info = if let Some(container_name) = &self.container_name {
            &container_statuses
                .iter()
                .find(|&status| &status.name == container_name)
                .ok_or_else(|| {
                    LayerError::ContainerNotFound(
                        container_name.clone(),
                        pod_namespace.to_string(),
                        self.pod_name.to_string(),
                    )
                })
                .unwrap()
                .container_id
        } else {
            info!("No container name specified, defaulting to first container found");
            &container_statuses.first().unwrap().container_id
        };
        container_info.clone()
    }
}

impl FromStr for Target {
    type Err = LayerError;

    fn from_str(target: &str) -> Result<Self> {
        target
            .parse::<DeploymentData>()
            .map(Target::Deployment)
            .or_else(|_| target.parse::<PodData>().map(Target::Pod))
    }
}
