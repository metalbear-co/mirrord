use std::{collections::HashSet, str::FromStr};

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::{
    apps::v1::Deployment,
    batch::v1::Job,
    core::v1::{ContainerStatus, EphemeralContainer, Pod},
};
use kube::{
    api::{Api, ListParams, LogParams, Portforwarder, PostParams},
    runtime::{watcher, WatchStreamExt},
    Client, Config,
};
use mirrord_config::LayerConfig;
use mirrord_progress::TaskProgress;
use rand::distributions::{Alphanumeric, DistString};
use serde_json::{json, to_vec};
use tokio::pin;
use tracing::{debug, warn};

use crate::{
    error::{LayerError, Result},
    MIRRORD_SKIP_LOAD,
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
        std::env::set_var(MIRRORD_SKIP_LOAD, "true");
        let library = std::env::var(EnvVarGuard::ENV_VAR).unwrap_or_default();
        std::env::remove_var(EnvVarGuard::ENV_VAR);
        Self { library }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        std::env::set_var(EnvVarGuard::ENV_VAR, &self.library);
        std::env::set_var(MIRRORD_SKIP_LOAD, "false");
    }
}

/// Return the agent image to use
fn agent_image(config: &LayerConfig) -> String {
    if let Some(image) = &config.agent.image {
        image.clone()
    } else {
        concat!("ghcr.io/metalbear-co/mirrord:", env!("CARGO_PKG_VERSION")).to_string()
    }
}

/// Return target based on layer config.
fn target(config: &LayerConfig) -> Result<Target> {
    if let Some(target) = &config.target {
        target.parse()
    } else if let Some(pod_name) = &config.pod.name {
        warn!("[WARNING]: DEPRECATED - `MIRRORD_AGENT_IMPERSONATED_POD_NAME` is deprecated, consider using `MIRRORD_IMPERSONATED_TARGET` instead.
        \nDeprecated since: [28/09/2022] | Scheduled removal: [28/10/2022]");
        // START | DEPRECATED: - Scheduled for removal on [28/10/2022]
        Ok(Target::Pod(PodTarget {
            pod_name: pod_name.clone(),
            container_name: config.pod.container.clone(),
        }))
    } else {
        Err(LayerError::InvalidTarget(
            "No target specified. Please set the `MIRRORD_IMPERSONATED_TARGET` environment variable.".to_string(),
        ))
    }
}

/// Choose container logic:
/// 1. Try to find based on given name
/// 2. Try to find first container in pod that isn't a mesh side car
/// 3. Take first container in pod
fn choose_container<'a>(
    container_name: &Option<String>,
    container_statuses: &'a [ContainerStatus],
) -> Option<&'a ContainerStatus> {
    if let Some(name) = container_name {
        container_statuses
            .iter()
            .find(|&status| &status.name == name)
    } else {
        let skip_names =
            HashSet::from(["istio-proxy", "linkerd-proxy", "proxy-init", "istio-init"]);
        // Choose any container that isn't part of the skip list
        container_statuses
            .iter()
            .find(|&status| !skip_names.contains(status.name.as_str()))
            .or_else(|| container_statuses.first())
    }
}

pub(crate) struct KubernetesAPI {
    client: Client,
    config: LayerConfig,
    _guard: EnvVarGuard,
}

impl KubernetesAPI {
    pub async fn new(config: &LayerConfig) -> Result<Self> {
        let _guard = EnvVarGuard::new();
        let client = if config.accept_invalid_certificates {
            let mut config = Config::infer().await?;
            config.accept_invalid_certs = true;
            warn!("Accepting invalid certificates");
            Client::try_from(config).map_err(LayerError::KubeError)?
        } else {
            Client::try_default().await.map_err(LayerError::KubeError)?
        };
        Ok(Self {
            client,
            config: config.clone(),
            _guard,
        })
    }

    /// Create a Kubernetes API subject to the namespace of the agent
    fn get_agent_api<K>(&self) -> Api<K>
    where
        K: kube::Resource,
        <K as kube::Resource>::DynamicType: Default,
    {
        if let Some(namespace) = &self.config.agent.namespace {
            Api::namespaced(self.client.clone(), namespace)
        } else {
            Api::default_namespaced(self.client.clone())
        }
    }

    /// Create a Kubernetes API subject to the namespace of the target pod.
    fn get_target_pod_api<K>(&self) -> Api<K>
    where
        K: kube::Resource,
        <K as kube::Resource>::DynamicType: Default,
    {
        if let Some(namespace) = &self.config.target_namespace {
            Api::namespaced(self.client.clone(), namespace)
        } else if let Some(namespace) = &self.config.pod.namespace {
            // START | DEPRECATED: - Scheduled for removal on [28/10/2022]
            Api::namespaced(self.client.clone(), namespace)
        } else {
            Api::default_namespaced(self.client.clone())
        }
    }

    async fn create_ephemeral_container_agent(
        &self,
        runtime_data: RuntimeData,
        agent_image: String,
        connection_port: u16,
        progress: &TaskProgress,
    ) -> Result<String> {
        let container_progress = progress.subtask("creating ephemeral container...");

        warn!("Ephemeral Containers is an experimental feature
                  >> Refer https://kubernetes.io/docs/concepts/workloads/pods/ephemeral-containers/ for more info");

        let mirrord_agent_name = get_agent_name();

        let mut agent_command_line = vec![
            "./mirrord-agent".to_string(),
            "-l".to_string(),
            connection_port.to_string(),
            "-e".to_string(),
        ];
        if let Some(timeout) = self.config.agent.communication_timeout {
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
            "imagePullPolicy": self.config.agent.image_pull_policy,
            "targetContainerName": runtime_data.container_name,
            "env": [{"name": "RUST_LOG", "value": self.config.agent.log_level}],
            "command": agent_command_line,
        }))?;
        debug!("Requesting ephemeral_containers_subresource");

        let pod_api = self.get_target_pod_api();
        let mut ephemeral_containers_subresource: Pod = pod_api
            .get_subresource("ephemeralcontainers", &runtime_data.pod_name)
            .await
            .map_err(LayerError::KubeError)?;

        let mut spec = ephemeral_containers_subresource
            .spec
            .as_mut()
            .ok_or(LayerError::PodSpecNotFound)?;

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

        container_progress.done_with("container created");

        let container_progress = progress.subtask("waiting for container to be ready...");

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

        wait_for_agent_startup(&pod_api, &runtime_data.pod_name, mirrord_agent_name).await?;

        container_progress.done_with("container is ready");

        debug!("container is ready");
        Ok(runtime_data.pod_name.to_string())
    }

    async fn create_job_pod_agent(
        &self,
        runtime_data: RuntimeData,
        agent_image: String,
        connection_port: u16,
        progress: &TaskProgress,
    ) -> Result<String> {
        let pod_progress = progress.subtask("creating agent pod...");
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
        if let Some(timeout) = self.config.agent.communication_timeout {
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
                            "sidecar.istio.io/inject": "false",
                            "linkerd.io/inject": "disabled"
                        }
                    },
                    "spec": {
                    "ttlSecondsAfterFinished": self.config.agent.ttl,

                        "template": {
                            "metadata": {
                                "annotations":
                                {
                                    "sidecar.istio.io/inject": "false",
                                    "linkerd.io/inject": "disabled"
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
                                "imagePullPolicy": self.config.agent.image_pull_policy,
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
                                "env": [{"name": "RUST_LOG", "value": self.config.agent.log_level}],
                                "resources": // Add requests to avoid getting defaulted https://github.com/metalbear-co/mirrord/issues/579
                                {
                                    "requests":
                                    {
                                        "cpu": "10m",
                                        "memory": "50Mi"
                                    }
                                }
                            }
                        ]
                    }
                }
            }
                }
            ))?;
        let job_api = self.get_agent_api();

        job_api
            .create(&PostParams::default(), &agent_pod)
            .await
            .map_err(LayerError::KubeError)?;

        let params = ListParams::default()
            .labels(&format!("job-name={}", mirrord_agent_job_name))
            .timeout(60);

        pod_progress.done_with("agent pod created");

        let pod_progress = progress.subtask("waiting for pod to be ready...");

        let pod_api: Api<Pod> = self.get_agent_api();

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

        wait_for_agent_startup(&pod_api, &pod_name, "mirrord-agent".to_string()).await?;

        pod_progress.done_with("pod is ready");

        Ok(pod_name)
    }

    pub(crate) async fn create_agent(&self, connection_port: u16) -> Result<String> {
        let progress = TaskProgress::new("agent initializing...");
        let agent_image = agent_image(&self.config);
        let runtime_data = self.get_runtime_data().await?;
        let connect_pod_name = if self.config.agent.ephemeral {
            self.create_ephemeral_container_agent(
                runtime_data,
                agent_image,
                connection_port,
                &progress,
            )
            .await?
        } else {
            self.create_job_pod_agent(runtime_data, agent_image, connection_port, &progress)
                .await?
        };
        progress.done_with("agent running");

        Ok(connect_pod_name)
    }

    pub(crate) async fn port_forward(&self, pod_name: &str, port: u16) -> Result<Portforwarder> {
        let pod_api: Api<Pod> = self.get_agent_api();
        pod_api
            .portforward(pod_name, &[port])
            .await
            .map_err(LayerError::KubeError)
    }
    async fn get_runtime_data(&self) -> Result<RuntimeData> {
        let target = target(&self.config)?;
        target.runtime_data(self).await
    }
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

pub(crate) struct RuntimeData {
    pod_name: String,
    node_name: String,
    container_id: String,
    container_runtime: String,
    container_name: String,
    socket_path: String,
}

impl RuntimeData {
    fn from_pod(pod: &Pod, container_name: &Option<String>) -> Result<Self> {
        let pod_name = pod
            .metadata
            .name
            .as_ref()
            .ok_or(LayerError::PodNameNotFound)?
            .to_owned();
        let node_name = pod
            .spec
            .as_ref()
            .ok_or(LayerError::PodSpecNotFound)?
            .node_name
            .as_ref()
            .ok_or(LayerError::NodeNotFound)?
            .to_owned();
        let container_statuses = pod
            .status
            .as_ref()
            .ok_or(LayerError::PodStatusNotFound)?
            .container_statuses
            .as_ref()
            .ok_or(LayerError::ContainerStatusNotFound)?;
        let chosen_status =
            choose_container(container_name, container_statuses).ok_or_else(|| {
                LayerError::ContainerNotFound(
                    container_name.clone().unwrap_or_else(|| "None".to_string()),
                )
            })?;

        let container_name = chosen_status.name.clone();
        let container_id_full = chosen_status
            .container_id
            .as_ref()
            .ok_or(LayerError::ContainerIdNotFound)?
            .to_owned();

        let mut split = container_id_full.split("://");

        let (container_runtime, socket_path) = match split.next() {
            Some("docker") => Ok(("docker", "/var/run/docker.sock")),
            Some("containerd") => Ok(("containerd", "/run/containerd/containerd.sock")),
            _ => Err(LayerError::ContainerRuntimeParseError(
                container_id_full.to_string(),
            )),
        }?;

        let container_id = split
            .next()
            .ok_or_else(|| LayerError::ContainerRuntimeParseError(container_id_full.to_string()))?
            .to_owned();

        let container_runtime = container_runtime.to_string();
        let socket_path = socket_path.to_string();

        Ok(RuntimeData {
            pod_name,
            node_name,
            container_id,
            container_runtime,
            container_name,
            socket_path,
        })
    }
}
// END

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeploymentTarget {
    pub deployment_name: String,
    pub container_name: Option<String>,
}

#[async_trait]
impl RuntimeDataProvider for DeploymentTarget {
    async fn runtime_data(&self, client: &KubernetesAPI) -> Result<RuntimeData> {
        let deployment_api: Api<Deployment> = client.get_target_pod_api();
        let deployment = deployment_api
            .get(&self.deployment_name)
            .await
            .map_err(LayerError::KubeError)?;

        let deployment_labels = deployment
            .spec
            .and_then(|spec| spec.selector.match_labels)
            .ok_or_else(|| {
                LayerError::DeploymentNotFound(format!(
                    "Label for deployment: {}, not found!",
                    self.deployment_name.clone()
                ))
            })?;

        // convert to key value pair
        let formatted_deployments_labels = deployment_labels
            .iter()
            .map(|(key, value)| format!("{}={}", key, value))
            .collect::<Vec<String>>()
            .join(",");

        let pod_api: Api<Pod> = client.get_target_pod_api();
        let deployment_pods = pod_api
            .list(&ListParams::default().labels(&formatted_deployments_labels))
            .await
            .map_err(LayerError::KubeError)?;

        let first_pod = deployment_pods.items.first().ok_or_else(|| {
            LayerError::DeploymentNotFound(format!(
                "Failed to fetch the default(first pod) from ObjectList<Pod> for {}",
                self.deployment_name.clone()
            ))
        })?;

        RuntimeData::from_pod(first_pod, &self.container_name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Target {
    Deployment(DeploymentTarget),
    Pod(PodTarget),
}

#[async_trait]
trait RuntimeDataProvider {
    async fn runtime_data(&self, client: &KubernetesAPI) -> Result<RuntimeData>;
}

#[async_trait]
impl RuntimeDataProvider for Target {
    async fn runtime_data(&self, client: &KubernetesAPI) -> Result<RuntimeData> {
        match self {
            Target::Deployment(deployment) => deployment.runtime_data(client).await,
            Target::Pod(pod) => pod.runtime_data(client).await,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PodTarget {
    pub pod_name: String,
    pub container_name: Option<String>,
}

#[async_trait]
impl RuntimeDataProvider for PodTarget {
    async fn runtime_data(&self, client: &KubernetesAPI) -> Result<RuntimeData> {
        let pod_api: Api<Pod> = client.get_target_pod_api();
        let pod = pod_api.get(&self.pod_name).await?;

        RuntimeData::from_pod(&pod, &self.container_name)
    }
}

trait FromSplit {
    fn from_split(split: &mut std::str::Split<char>) -> Result<Self>
    where
        Self: Sized;
}

impl FromSplit for DeploymentTarget {
    fn from_split(split: &mut std::str::Split<char>) -> Result<Self> {
        let deployment_name = split.next().ok_or_else(|| {
            LayerError::InvalidTarget("Deployment target must have a deployment name".to_string())
        })?;
        match (split.next(), split.next()) {
            (Some("container"), Some(container_name)) => {
                Ok(Self {
                    deployment_name: deployment_name.to_string(),
                    container_name: Some(container_name.to_string()),
                })
            },
            (None, None) => Ok(Self {
                deployment_name: deployment_name.to_string(),
                container_name: None,
            }),
            _ => Err(LayerError::InvalidTarget(
                "Deployment target must be in the format deployment/<deployment_name>[/container/<container_name>]"
                    .to_string(),
            )),
        }
    }
}

impl FromSplit for PodTarget {
    fn from_split(split: &mut std::str::Split<char>) -> Result<Self> {
        let pod_name = split.next().ok_or_else(|| {
            LayerError::InvalidTarget("Deployment target must have a deployment name".to_string())
        })?;
        match (split.next(), split.next()) {
            (Some("container"), Some(container_name)) => Ok(Self {
                pod_name: pod_name.to_string(),
                container_name: Some(container_name.to_string()),
            }),
            (None, None) => Ok(Self {
                pod_name: pod_name.to_string(),
                container_name: None,
            }),
            _ => Err(LayerError::InvalidTarget(
                "Pod target must be in the format pod/<pod_name>[/container/<container_name>]"
                    .to_string(),
            )),
        }
    }
}

impl FromStr for Target {
    type Err = LayerError;

    fn from_str(target: &str) -> Result<Target> {
        let mut split = target.split('/');
        match split.next() {
            Some("deployment") | Some("deploy") => {
                DeploymentTarget::from_split(&mut split).map(Target::Deployment)
            }
            Some("pod") => PodTarget::from_split(&mut split).map(Target::Pod),
            _ => Err(LayerError::InvalidTarget(format!(
                "Provided target: {:?} is neither a pod or a deployment.",
                target
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("pod/foobaz", Target::Pod(PodTarget {pod_name: "foobaz".to_string(), container_name: None}))]
    #[case("deployment/foobaz", Target::Deployment(DeploymentTarget {deployment_name: "foobaz".to_string(), container_name: None}))]
    #[case("deployment/nginx-deployment", Target::Deployment(DeploymentTarget {deployment_name: "nginx-deployment".to_string(), container_name: None}))]
    #[case("pod/foo/container/baz", Target::Pod(PodTarget { pod_name: "foo".to_string(), container_name: Some("baz".to_string()) }))]
    #[case("deployment/nginx-deployment/container/container-name", Target::Deployment(DeploymentTarget {deployment_name: "nginx-deployment".to_string(), container_name: Some("container-name".to_string())}))]
    fn test_target_parses(#[case] target: &str, #[case] expected: Target) {
        let target = target.parse::<Target>().unwrap();
        assert_eq!(target, expected)
    }

    #[rstest]
    #[should_panic(expected = "InvalidTarget")]
    #[case::panic("deployment/foobaz/blah")]
    #[should_panic(expected = "InvalidTarget")]
    #[case::panic("pod/foo/baz")]
    fn test_target_parse_fails(#[case] target: &str) {
        let target = target.parse::<Target>().unwrap();
        assert_eq!(
            target,
            Target::Deployment(DeploymentTarget {
                deployment_name: "foobaz".to_string(),
                container_name: None
            })
        )
    }
}
