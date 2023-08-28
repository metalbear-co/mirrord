use futures::StreamExt;
use k8s_openapi::api::core::v1::{EphemeralContainer as KubeEphemeralContainer, Pod};
use kube::{
    api::PostParams,
    runtime::{watcher, WatchStreamExt},
    Client,
};
use mirrord_config::agent::AgentConfig;
use mirrord_progress::Progress;
use serde_json::json;
use tokio::pin;
use tracing::{debug, warn};

use crate::{
    api::{
        container::{
            util::{get_agent_image, get_capabilities, wait_for_agent_startup},
            ContainerParams, ContainerVariant,
        },
        kubernetes::{get_k8s_resource_api, AgentKubernetesConnectInfo},
        runtime::RuntimeData,
    },
    error::{KubeApiError, Result},
};

fn is_ephemeral_container_running(pod: Pod, container_name: &str) -> bool {
    debug!("pod status: {:?}", &pod.status);
    pod.status
        .and_then(|status| {
            status
                .ephemeral_container_statuses
                .and_then(|container_statuses| {
                    container_statuses
                        .iter()
                        .find(|&status| status.name == container_name)
                        .and_then(|status| {
                            status.state.as_ref().map(|state| state.running.is_some())
                        })
                })
        })
        .unwrap_or(false)
}

pub async fn create_ephemeral_agent<P, V>(
    client: &Client,
    agent: &AgentConfig,
    params: &ContainerParams,
    runtime_data: &RuntimeData,
    variant: &V,
    progress: &P,
) -> Result<AgentKubernetesConnectInfo>
where
    P: Progress + Send + Sync,
    V: ContainerVariant<Update = KubeEphemeralContainer>,
{
    // Ephemeral should never be targetless, so there should be runtime data.
    let mut container_progress = progress.subtask("creating ephemeral container...");

    warn!("Ephemeral Containers is an experimental feature
                  >> Refer https://kubernetes.io/docs/concepts/workloads/pods/ephemeral-containers/ for more info");

    let mut ephemeral_container: KubeEphemeralContainer = variant.as_update(agent)?;
    debug!("Requesting ephemeral_containers_subresource");

    let pod_api = get_k8s_resource_api(client, runtime_data.pod_namespace.as_deref());
    let pod: Pod = pod_api.get(&runtime_data.pod_name).await?;
    let pod_spec = pod.spec.ok_or(KubeApiError::PodSpecNotFound)?;

    let container_spec = pod_spec
        .containers
        .iter()
        .find(|c| c.name == runtime_data.container_name)
        .ok_or_else(|| KubeApiError::ContainerNotFound(runtime_data.container_name.clone()))?;

    if let Some(spec_env) = container_spec.env.as_ref() {
        let mut env = ephemeral_container.env.unwrap_or_default();
        env.extend(spec_env.iter().cloned());
        ephemeral_container.env = Some(env)
    }

    if let Some(env_from) = container_spec.env_from.as_ref() {
        let mut env = ephemeral_container.env_from.unwrap_or_default();
        env.extend(env_from.iter().cloned());
        ephemeral_container.env_from = Some(env)
    }

    let mut ephemeral_containers_subresource: Pod = pod_api
        .get_subresource("ephemeralcontainers", &runtime_data.pod_name)
        .await
        .map_err(KubeApiError::KubeError)?;

    let mut spec = ephemeral_containers_subresource
        .spec
        .as_mut()
        .ok_or(KubeApiError::PodSpecNotFound)?;

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
            serde_json::to_vec(&ephemeral_containers_subresource).map_err(KubeApiError::from)?,
        )
        .await
        .map_err(KubeApiError::KubeError)?;

    let watcher_config = watcher::Config::default()
        .fields(&format!("metadata.name={}", &runtime_data.pod_name))
        .timeout(60);

    container_progress.success(Some("container created"));

    let mut container_progress = progress.subtask("waiting for container to be ready...");

    let stream = watcher(pod_api.clone(), watcher_config).applied_objects();
    pin!(stream);

    while let Some(Ok(pod)) = stream.next().await {
        if is_ephemeral_container_running(pod, &params.name) {
            debug!("container ready");
            break;
        } else {
            debug!("container not ready yet");
        }
    }

    let version =
        wait_for_agent_startup(&pod_api, &runtime_data.pod_name, params.name.clone()).await?;
    match version {
        Some(version) if version != env!("CARGO_PKG_VERSION") => {
            let message = format!(
                "Agent version {version} does not match the local mirrord version {}. This may lead to unexpected errors.",
                env!("CARGO_PKG_VERSION"),
            );
            container_progress.warning(&message);
        }
        _ => {}
    }

    container_progress.success(Some("container is ready"));

    debug!("container is ready");
    Ok(AgentKubernetesConnectInfo {
        pod_name: runtime_data.pod_name.to_string(),
        agent_port: params.port,
        namespace: runtime_data.pod_namespace.clone(),
    })
}

pub struct EphemeralTargetedVariant<'c> {
    command_line: Vec<String>,
    params: &'c ContainerParams,
    runtime_data: &'c RuntimeData,
}

impl<'c> EphemeralTargetedVariant<'c> {
    pub fn new(params: &'c ContainerParams, runtime_data: &'c RuntimeData) -> Self {
        let command_line = vec![
            "./mirrord-agent".to_string(),
            "-l".to_string(),
            params.port.to_string(),
            "-e".to_string(),
        ];

        EphemeralTargetedVariant {
            params,
            command_line,
            runtime_data,
        }
    }
}

impl ContainerVariant for EphemeralTargetedVariant<'_> {
    type Update = KubeEphemeralContainer;

    fn as_update(&self, agent: &AgentConfig) -> Result<KubeEphemeralContainer> {
        let EphemeralTargetedVariant {
            params,
            runtime_data,
            command_line,
            ..
        } = self;

        let mut command_line = command_line.clone();

        if let Some(timeout) = agent.communication_timeout {
            command_line.push("-t".to_owned());
            command_line.push(timeout.to_string());
        }

        #[cfg(debug_assertions)]
        if agent.test_error {
            command_line.push("--test-error".to_owned());
        }

        serde_json::from_value(json!({
            "name": params.name,
            "image": get_agent_image(agent),
            "securityContext": {
                "runAsGroup": params.gid,
                "capabilities": {
                    "add": get_capabilities(agent),
                },
                "privileged": agent.privileged,
            },
            "imagePullPolicy": agent.image_pull_policy,
            "targetContainerName": runtime_data.container_name,
            "env": [
                {"name": "RUST_LOG", "value": agent.log_level},
                { "name": "MIRRORD_AGENT_STEALER_FLUSH_CONNECTIONS", "value": agent.flush_connections.to_string() }
            ],
            "command": command_line,
        })).map_err(KubeApiError::from)
    }
}
