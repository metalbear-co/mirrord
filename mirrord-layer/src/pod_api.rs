use std::str::FromStr;

use async_trait::async_trait;
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
use mirrord_config::LayerConfig;
use rand::distributions::{Alphanumeric, DistString};
use serde_json::{json, to_vec};
use tokio::pin;
use tracing::{debug, info, warn};

use crate::error::{LayerError, Result};

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

pub(crate) async fn create_agent(
    config: LayerConfig,
    connection_port: u16,
) -> Result<Portforwarder> {
    let _guard = EnvVarGuard::new();
    let LayerConfig {
        target,
        target_namespace,
        agent,
        pod,
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

    let agent_image = agent.image.unwrap_or_else(|| {
        concat!("ghcr.io/metalbear-co/mirrord:", env!("CARGO_PKG_VERSION")).to_string()
    });

    // START | DEPRECATED: - Scheduled for removal on [28/10/2022]
    let (runtime_data, pod_api): (RuntimeData, Api<Pod>) = match (&target, &pod.name) {
        (Some(target), None) => (
            target
                .parse::<Target>()?
                .runtime_data(&client, &target_namespace)
                .await?,
            Api::namespaced(
                client.clone(),
                agent.namespace.as_ref().unwrap_or(&target_namespace),
            ),
        ),
        (None, Some(pod_name)) => {
            warn!("[WARNING]: DEPRECATED - `MIRRORD_AGENT_IMPERSONATED_POD_NAME` is deprecated, consider using `MIRRORD_IMPERSONATED_TARGET` instead.
                \nDeprecated since: [28/09/2022] | Scheduled removal: [28/10/2022]");
            (
                RuntimeData::from_k8s(&client, pod_name, &pod.namespace, &pod.container).await?,
                Api::namespaced(
                    client.clone(),
                    agent.namespace.as_ref().unwrap_or(&pod.namespace),
                ),
            )
        }
        _ => unreachable!(),
    };
    // END

    let pod_name = if agent.ephemeral {
        create_ephemeral_container_agent(
            &config,
            runtime_data,
            &pod_api,
            agent_image,
            connection_port,
        )
        .await?
    } else {
        // START | DEPRECATED: - Scheduled for removal on [28/10/2022]
        let job_api: Api<Job> = match target {
            Some(_) => Api::namespaced(
                client.clone(),
                agent.namespace.as_ref().unwrap_or(&target_namespace),
            ),
            None if pod.name.is_some() => Api::namespaced(
                client.clone(),
                agent.namespace.as_ref().unwrap_or(&pod.namespace),
            ),
            None => unreachable!(),
        };
        // END

        create_job_pod_agent(
            &config,
            agent_image,
            &pod_api,
            runtime_data,
            &job_api,
            connection_port,
        )
        .await?
    };
    pod_api
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
    pod_api: &Api<Pod>,
    pod_name: &str,
    container_name: String,
) -> Result<()> {
    let mut logs = pod_api
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
    runtime_data: RuntimeData,
    pod_api: &Api<Pod>,
    agent_image: String,
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
    if let Some(timeout) = config.agent.communication_timeout {
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
        "imagePullPolicy": config.agent.image_pull_policy,
        "targetContainerName": runtime_data.container_name,
        "env": [{"name": "RUST_LOG", "value": config.agent.log_level}],
        "command": agent_command_line,
    }))?;
    debug!("Requesting ephemeral_containers_subresource");

    let mut ephemeral_containers_subresource = pod_api
        .get_subresource("ephemeralcontainers", &runtime_data.pod_name)
        .await
        .map_err(LayerError::KubeError)?;

    let mut spec = ephemeral_containers_subresource
        .spec
        .as_mut()
        .ok_or_else(|| LayerError::PodSpecNotFound(runtime_data.pod_name.clone()))?;

    spec.ephemeral_containers = match spec.ephemeral_containers.clone() {
        Some(mut ephemeral_containers) => {
            ephemeral_containers.push(ephemeral_container);
            Some(ephemeral_containers)
        }
        None => Some(vec![ephemeral_container]),
    };

    pod_api
        .replace_subresource(
            "ephemeralcontainers",
            &runtime_data.pod_name,
            &PostParams::default(),
            to_vec(&ephemeral_containers_subresource).map_err(LayerError::from)?,
        )
        .await
        .map_err(LayerError::KubeError)?;

    let params = ListParams::default()
        .fields(&format!("metadata.name={}", &runtime_data.pod_name))
        .timeout(60);

    let stream = watcher(pod_api.clone(), params).applied_objects();
    pin!(stream);

    while let Some(Ok(pod)) = stream.next().await {
        if is_ephemeral_container_running(pod, &mirrord_agent_name) {
            debug!("container ready");
            break;
        } else {
            debug!("container not ready yet");
        }
    }

    wait_for_agent_startup(pod_api, &runtime_data.pod_name, mirrord_agent_name).await?;

    debug!("container is ready");
    Ok(runtime_data.pod_name.to_string())
}

async fn create_job_pod_agent(
    config: &LayerConfig,
    agent_image: String,
    pod_api: &Api<Pod>,
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
    if let Some(timeout) = config.agent.communication_timeout {
        agent_command_line.push("-t".to_string());
        agent_command_line.push(timeout.to_string());
    }

    let agent_pod: Job =
        serde_json::from_value(json!({ // Only Jobs support self deletion after completion
                "metadata": {
                    "name": mirrord_agent_job_name,
                    "labels": {
                        "app": "mirrord"
                    },
                    "annotations":
                    {
                        "sidecar.istio.io/inject": "false"
                    }
                },
                "spec": {
                "ttlSecondsAfterFinished": config.agent.ttl,

                    "template": {
                        "metadata": {
                            "annotations":
                            {
                                "sidecar.istio.io/inject": "false"
                            }
                        },

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
                            "imagePullPolicy": config.agent.image_pull_policy,
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
                            "env": [{"name": "RUST_LOG", "value": config.agent.log_level}],
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

    let stream = watcher(pod_api.clone(), params).applied_objects();
    pin!(stream);

    while let Some(Ok(pod)) = stream.next().await {
        if let Some(status) = &pod.status && let Some(phase) = &status.phase {
                    debug!("Pod Phase = {phase:?}");
                if phase == "Running" {
                    break;
                }
            }
    }

    let pods = pod_api
        .list(&ListParams::default().labels(&format!("job-name={}", mirrord_agent_job_name)))
        .await
        .map_err(LayerError::KubeError)?;

    let pod_name = pods
        .items
        .first()
        .and_then(|pod| pod.metadata.name.clone())
        .ok_or(LayerError::JobPodNotFound(mirrord_agent_job_name))?;

    wait_for_agent_startup(pod_api, &pod_name, "mirrord-agent".to_string()).await?;
    Ok(pod_name)
}

pub(crate) struct RuntimeData {
    pod_name: String,
    node_name: String,
    container_id: String,
    container_runtime: String,
    container_name: String,
    socket_path: String,
}

// START | DEPRECATED: - Scheduled for removal on [28/10/2022]
impl RuntimeData {
    async fn from_k8s(
        client: &Client,
        pod_name: &str,
        pod_namespace: &str,
        container_name: &Option<String>,
    ) -> Result<Self> {
        let pod_api: Api<Pod> = Api::namespaced(client.clone(), pod_namespace);
        let pod = pod_api.get(pod_name).await?;
        let node_name = &pod.spec.unwrap().node_name;
        let container_statuses = &pod.status.unwrap().container_statuses.unwrap();
        let container_info = if let Some(container_name) = container_name {
            &container_statuses
                .iter()
                .find(|&status| &status.name == container_name)
                .ok_or_else(|| LayerError::ContainerNotFound(container_name.clone()))?
                .container_id
        } else {
            info!("No container name specified, defaulting to first container found");
            &container_statuses.first().unwrap().container_id
        };

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

        let container_name = container_name
            .as_ref()
            .unwrap_or_else(|| &container_statuses.first().unwrap().name);

        Ok(RuntimeData {
            container_id: container_id.to_string(),
            container_runtime: container_runtime.to_string(),
            node_name: node_name.as_ref().unwrap().to_string(),
            socket_path: socket_path.to_string(),
            pod_name: pod_name.to_string(),
            container_name: container_name.clone(),
        })
    }
}
// END

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeploymentData {
    pub deployment: String,
}

#[async_trait]
impl RuntimeDataProvider for DeploymentData {
    async fn runtime_data(&self, client: &Client, namespace: &str) -> Result<RuntimeData> {
        let deployment_api: Api<Deployment> = Api::namespaced(client.clone(), namespace);
        let deployment = deployment_api
            .get(&self.deployment)
            .await
            .map_err(LayerError::KubeError)?;

        let pod_label = deployment
            .spec
            .and_then(|spec| spec.template.metadata)
            .and_then(|metadata| metadata.labels)
            .and_then(|labels| labels.get("app").cloned())
            .ok_or_else(|| {
                LayerError::DeploymentNotFound(format!(
                    "Label for deployment: {}, not found!",
                    self.deployment.clone()
                ))
            })?;

        let pod_api: Api<Pod> = Api::namespaced(client.clone(), namespace);
        let deployment_pods = pod_api
            .list(&ListParams::default().labels(&format!("app={}", pod_label)))
            .await
            .map_err(LayerError::KubeError)?;

        let first_pod = deployment_pods.items.first().ok_or_else(|| {
            LayerError::DeploymentNotFound(format!(
                "Failed to fetch the default(first pod) from ObjectList<Pod> for {}",
                self.deployment.clone()
            ))
        })?;

        let pod_name = first_pod.clone().metadata.name.ok_or_else(|| {
            LayerError::DeploymentNotFound(format!(
                "Failed to fetch the name of the default pod in deployment {}, pod {:?}",
                self.deployment.clone(),
                first_pod.clone()
            ))
        })?;

        let (
            container_name,
            ContainerData {
                container_runtime,
                socket_path,
                container_id,
            },
        ) = || -> Option<(String, String)> {
            let container_name = first_pod
                .clone()
                .spec?
                .containers
                .first()
                .map(|container| container.name.clone())?;
            let container_runtime_and_id = first_pod
                .clone()
                .status?
                .container_statuses?
                .first()?
                .container_id
                .clone()?;

            Some((container_name, container_runtime_and_id))
        }()
        .ok_or_else(|| {
            LayerError::ContainerNotFound(format!(
                "Failed to fetch container for pod {:?}, deployment {}",
                first_pod.clone(),
                self.deployment.clone()
            ))
        })
        .and_then(|(container_name, container_runtime_and_id)| {
            Ok((
                container_name,
                container_runtime_and_id.parse::<ContainerData>()?,
            ))
        })?;

        let node_name = first_pod
            .clone()
            .spec
            .and_then(|spec| spec.node_name)
            .ok_or_else(|| {
                LayerError::NodeNotFound(format!(
                    "Target: {:?} | Pod: {:?} | Container: {}",
                    self.clone(),
                    first_pod.clone(),
                    container_name.clone()
                ))
            })?;

        Ok(RuntimeData {
            node_name,
            pod_name,
            container_name,
            container_id,
            container_runtime,
            socket_path,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Target {
    Deployment(DeploymentData),
    Pod(PodData),
}

#[async_trait]
trait RuntimeDataProvider {
    async fn runtime_data(&self, client: &Client, namespace: &str) -> Result<RuntimeData>;
}

impl Target {
    pub async fn runtime_data(&self, client: &Client, namespace: &str) -> Result<RuntimeData> {
        match self {
            Target::Pod(pod) => pod.runtime_data(client, namespace).await,
            Target::Deployment(deployment) => deployment.runtime_data(client, namespace).await,
        }
    }
}

impl FromStr for DeploymentData {
    type Err = LayerError;

    fn from_str(input: &str) -> Result<Self> {
        let target_data = input.split('/').collect::<Vec<&str>>();
        match target_data.first() {
            Some(&"deployment") if target_data.len() == 2 => Ok(DeploymentData {
                deployment: target_data[1].to_string(),
            }),
            _ => Err(LayerError::InvalidTarget(format!(
                "Provided target: {:?} is not a deployment.",
                input
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PodData {
    pub pod_name: String,
    pub container_name: Option<String>,
}

impl FromStr for PodData {
    type Err = LayerError;

    fn from_str(input: &str) -> Result<Self> {
        let target_data = input.split('/').collect::<Vec<&str>>();
        match target_data.first() {
            Some(&"pod") if target_data.len() == 2 => Ok(PodData {
                pod_name: target_data[1].to_string(),
                container_name: None,
            }),
            Some(&"pod") if target_data.len() == 4 && target_data[2] == "container" => {
                Ok(PodData {
                    pod_name: target_data[1].to_string(),
                    container_name: Some(target_data[3].to_string()),
                })
            }
            _ => Err(LayerError::InvalidTarget(format!(
                "Provided target: {:?} is neither a pod or a deployment.",
                input
            ))),
        }
    }
}

#[async_trait]
impl RuntimeDataProvider for PodData {
    async fn runtime_data(&self, client: &Client, namespace: &str) -> Result<RuntimeData> {
        let pod_api: Api<Pod> = Api::namespaced(client.clone(), namespace);
        let pod = pod_api.get(&self.pod_name).await?;
        let (
            container_name,
            ContainerData {
                container_runtime,
                socket_path,
                container_id,
            },
        ) = || -> Option<(String, String)> {
            let container_statuses = &pod.clone().status?.container_statuses?;
            let container_name = if let Some(container_name) = self.container_name.clone() {
                container_name
            } else {
                pod.clone()
                    .spec?
                    .containers
                    .first()
                    .map(|container| container.name.clone())?
            };
            let container_runtime_and_id = container_statuses
                .iter()
                .find(|&status| status.name == container_name)?
                .container_id
                .clone()?;
            Some((container_name, container_runtime_and_id))
        }()
        .ok_or_else(|| {
            LayerError::ContainerNotFound(format!(
                "Failed to fetch container for pod {:#?}",
                self.clone(),
            ))
        })
        .and_then(|(container_name, container_runtime_and_id)| {
            Ok((
                container_name,
                container_runtime_and_id.parse::<ContainerData>()?,
            ))
        })?;

        let node_name = pod
            .spec
            .and_then(|spec| spec.node_name)
            .ok_or_else(|| LayerError::NodeNotFound(format!("{:?}", self.clone())))?;

        Ok(RuntimeData {
            container_id,
            container_runtime,
            node_name,
            socket_path,
            pod_name: self.pod_name.clone(),
            container_name,
        })
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

pub(crate) struct ContainerData {
    container_runtime: String,
    socket_path: String,
    container_id: String,
}

impl FromStr for ContainerData {
    type Err = LayerError;
    fn from_str(input: &str) -> Result<Self> {
        let container_runtime_and_id = input.split("://").collect::<Vec<&str>>();
        let (container_runtime, socket_path) = match container_runtime_and_id.first() {
            Some(&"docker") => ("docker", "/var/run/docker.sock"),
            Some(&"containerd") => ("containerd", "/run/containerd/containerd.sock"),
            _ => {
                return Err(LayerError::ContainerRuntimeParseError(
                    "unsupported container runtime".to_owned(),
                ))
            }
        };

        let container_id = container_runtime_and_id.last().ok_or_else(|| {
            LayerError::ContainerRuntimeParseError(format!(
                "Failed while parsing container_id for {}",
                input
            ))
        })?;

        Ok(ContainerData {
            container_runtime: container_runtime.to_string(),
            socket_path: socket_path.to_string(),
            container_id: container_id.to_string(),
        })
    }
}
