use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
};

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        batch::v1::Job,
        core::v1::{ContainerStatus, EphemeralContainer, Pod},
    },
    NamespaceResourceScope,
};
use kube::{
    api::{Api, ListParams, LogParams, Portforwarder, PostParams},
    runtime::{watcher, WatchStreamExt},
    Client, Config,
};
use mirrord_config::{
    target::{DeploymentTarget, PodTarget, Target},
    LayerConfig,
};
use mirrord_progress::TaskProgress;
use rand::distributions::{Alphanumeric, DistString};
use serde_json::{json, to_vec};
use tokio::pin;
use tracing::{debug, warn};

use crate::error::{LayerError, Result};

const MIRRORD_GUARDED_ENVS: &str = "MIRRORD_GUARDED_ENVS";

#[derive(Debug)]
pub(crate) struct EnvVarGuard {
    envs: HashMap<String, String>,
}

impl EnvVarGuard {
    #[cfg(target_os = "linux")]
    const ENV_VAR: &str = "LD_PRELOAD";
    #[cfg(target_os = "macos")]
    const ENV_VAR: &str = "DYLD_INSERT_LIBRARIES";

    fn new() -> Self {
        let envs = std::env::var(MIRRORD_GUARDED_ENVS)
            .ok()
            .and_then(|orig| serde_json::from_str(&orig).ok())
            .unwrap_or_else(|| {
                let fork_args = std::env::vars()
                    .filter(|(key, _)| key != MIRRORD_GUARDED_ENVS && key != EnvVarGuard::ENV_VAR)
                    .collect();

                if let Ok(ser_args) = serde_json::to_string(&fork_args) {
                    std::env::set_var(MIRRORD_GUARDED_ENVS, ser_args);
                }

                fork_args
            });

        Self { envs }
    }

    fn kube_env(&self) -> Vec<HashMap<String, String>> {
        let mut envs = Vec::new();
        self.extend_kube_env(&mut envs);
        envs
    }

    fn extend_kube_env(&self, envs: &mut Vec<HashMap<String, String>>) {
        let filtered: HashSet<_> = envs
            .iter()
            .filter_map(|item| item.get("name"))
            .cloned()
            .collect();

        for (key, value) in self
            .envs
            .iter()
            .filter(|(key, _)| !filtered.contains(key.as_str()))
        {
            let mut env = HashMap::new();
            env.insert("name".to_owned(), key.clone());
            env.insert("value".to_owned(), value.clone());

            envs.push(env);
        }
    }

    fn dropped_env(&self) -> Vec<String> {
        std::env::vars()
            .map(|(key, _)| key)
            .filter(|key| !self.envs.contains_key(key.as_str()))
            .collect()
    }

    #[cfg(test)]
    fn is_correct_auth_env(&self, envs: &HashMap<String, String>) -> Result<(), std::io::Error> {
        for (key, value) in envs {
            let orig_val = self.envs.get(key).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Missing env value {}", key),
                )
            })?;

            if value != orig_val {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!(
                        "Missmatch in values recived {} expected {}",
                        value, orig_val
                    ),
                ));
            }
        }

        Ok(())
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
    if let Some(target) = &config.target.path {
        Ok(target.clone())
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
}

impl KubernetesAPI {
    pub(crate) async fn create_kube_config(config: &LayerConfig) -> Result<Config> {
        if config.accept_invalid_certificates {
            let mut config = Config::infer().await?;
            config.accept_invalid_certs = true;
            warn!("Accepting invalid certificates");
            Ok(config)
        } else {
            Config::infer().await.map_err(|err| err.into())
        }
    }

    pub(crate) fn prepare_config(config: &mut Config, env_guard: &EnvVarGuard) {
        if let Some(mut exec) = config.auth_info.exec.as_mut() {
            match &mut exec.env {
                Some(envs) => env_guard.extend_kube_env(envs),
                None => exec.env = Some(env_guard.kube_env()),
            }

            exec.drop_env = Some(env_guard.dropped_env());
        }
        if let Some(mut auth_provider) = config.auth_info.auth_provider {
            if auth_provider.config.contains_key("cmd-path") {
                auth_provider.config.insert(
                    "cmd-drop-env".to_string(),
                    env_guard.dropped_env().join(" ").into(),
                );
            }
        }
    }

    pub async fn new(config: &LayerConfig) -> Result<Self> {
        let guard = EnvVarGuard::new();
        let mut kube_config = Self::create_kube_config(config).await?;
        Self::prepare_config(&mut kube_config, &guard);

        let client = Client::try_from(kube_config).map_err(LayerError::KubeError)?;

        Ok(Self {
            client,
            config: config.clone(),
        })
    }

    /// Create a Kubernetes API subject to the namespace of the agent
    fn get_agent_api<K>(&self) -> Api<K>
    where
        K: kube::Resource<Scope = NamespaceResourceScope>,
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
        K: kube::Resource<Scope = NamespaceResourceScope>,
        <K as kube::Resource>::DynamicType: Default,
    {
        if let Some(namespace) = &self.config.target.namespace {
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
            runtime_data.container_runtime.to_string(),
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
                                    "path": runtime_data.container_runtime.mount_path()
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
                                        "mountPath": runtime_data.container_runtime.mount_path(),
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

pub(crate) enum ContainerRuntime {
    Docker,
    Containerd,
}

impl ContainerRuntime {
    pub(crate) fn mount_path(&self) -> String {
        match self {
            ContainerRuntime::Docker => "/var/run/docker.sock".to_string(),
            ContainerRuntime::Containerd => "/run/".to_string(),
        }
    }
}

impl Display for ContainerRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContainerRuntime::Docker => write!(f, "docker"),
            ContainerRuntime::Containerd => write!(f, "containerd"),
        }
    }
}

pub(crate) struct RuntimeData {
    pod_name: String,
    node_name: String,
    container_id: String,
    container_runtime: ContainerRuntime,
    container_name: String,
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

        let container_runtime = match split.next() {
            Some("docker") => Ok(ContainerRuntime::Docker),
            Some("containerd") => Ok(ContainerRuntime::Containerd),
            _ => Err(LayerError::ContainerRuntimeParseError(
                container_id_full.to_string(),
            )),
        }?;

        let container_id = split
            .next()
            .ok_or_else(|| LayerError::ContainerRuntimeParseError(container_id_full.to_string()))?
            .to_owned();

        Ok(RuntimeData {
            pod_name,
            node_name,
            container_id,
            container_runtime,
            container_name,
        })
    }
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

#[async_trait]
impl RuntimeDataProvider for DeploymentTarget {
    async fn runtime_data(&self, client: &KubernetesAPI) -> Result<RuntimeData> {
        let deployment_api: Api<Deployment> = client.get_target_pod_api();
        let deployment = deployment_api
            .get(&self.deployment)
            .await
            .map_err(LayerError::KubeError)?;

        let deployment_labels = deployment
            .spec
            .and_then(|spec| spec.selector.match_labels)
            .ok_or_else(|| {
                LayerError::DeploymentNotFound(format!(
                    "Label for deployment: {}, not found!",
                    self.deployment.clone()
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
                self.deployment.clone()
            ))
        })?;

        RuntimeData::from_pod(first_pod, &self.container)
    }
}

#[async_trait]
impl RuntimeDataProvider for PodTarget {
    async fn runtime_data(&self, client: &KubernetesAPI) -> Result<RuntimeData> {
        let pod_api: Api<Pod> = client.get_target_pod_api();
        let pod = pod_api.get(&self.pod).await?;

        RuntimeData::from_pod(&pod, &self.container)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use http_body::Empty;
    use hyper::body::Body;
    use k8s_openapi::http::{header::AUTHORIZATION, Request, Response};
    use kube::{
        client::ConfigExt,
        config::{AuthInfo, ExecConfig},
    };
    use rstest::rstest;
    use tower::{service_fn, ServiceBuilder};

    use super::*;

    #[rstest]
    #[case("pod/foobaz", Target::Pod(PodTarget {pod: "foobaz".to_string(), container: None}))]
    #[case("deployment/foobaz", Target::Deployment(DeploymentTarget {deployment: "foobaz".to_string(), container: None}))]
    #[case("deployment/nginx-deployment", Target::Deployment(DeploymentTarget {deployment: "nginx-deployment".to_string(), container: None}))]
    #[case("pod/foo/container/baz", Target::Pod(PodTarget { pod: "foo".to_string(), container: Some("baz".to_string()) }))]
    #[case("deployment/nginx-deployment/container/container-name", Target::Deployment(DeploymentTarget {deployment: "nginx-deployment".to_string(), container: Some("container-name".to_string())}))]
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
                deployment: "foobaz".to_string(),
                container: None
            })
        )
    }

    #[tokio::test]
    async fn correct_envs_kubectl() {
        std::env::set_var("MIRRORD_TEST_ENV_VAR_KUBECTL", "true");

        let _guard = EnvVarGuard::new();
        let mut config = Config {
            accept_invalid_certs: true,
            auth_info: AuthInfo {
                exec: Some(ExecConfig {
                    api_version: Some("client.authentication.k8s.io/v1beta1".to_owned()),
                    command: "node".to_owned(),
                    args: Some(vec!["./tests/apps/kubectl/auth-util.js".to_owned()]),
                    env: None,
                    drop_env: None,
                }),
                ..Default::default()
            },
            ..Config::new("https://kubernetes.docker.internal:6443".parse().unwrap())
        };

        KubernetesAPI::prepare_config(&mut config, &_guard);

        let _guard = Arc::new(_guard);

        let service = ServiceBuilder::new()
            .layer(config.base_uri_layer())
            .option_layer(config.auth_layer().unwrap())
            .service(service_fn(move |req: Request<Body>| {
                let _guard = _guard.clone();
                async move {
                    let auth_env_vars = req
                        .headers()
                        .get(AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|token| token.strip_prefix("Bearer "))
                        .and_then(|token| base64::decode(token).ok())
                        .and_then(|value| {
                            serde_json::from_slice::<HashMap<String, String>>(&value).ok()
                        })
                        .ok_or_else(|| {
                            std::io::Error::new(std::io::ErrorKind::Other, "No Auth Header Sent")
                        })?;

                    _guard
                        .is_correct_auth_env(&auth_env_vars)
                        .map(|_| Response::new(Empty::new()))
                }
            }));

        let client = Client::new(service, config.default_namespace);

        std::env::set_var("MIRRORD_TEST_ENV_VAR_KUBECTL", "false");

        client.send(Request::new(Body::empty())).await.unwrap();
    }
}
