use futures::StreamExt;
use k8s_openapi::api::core::v1::{
    Capabilities, EphemeralContainer as KubeEphemeralContainer, Pod, SecurityContext,
};
use kube::{
    api::PostParams,
    runtime::{watcher, WatchStreamExt},
    Api, Client,
};
use mirrord_agent_env::{envs, mesh::MeshVendor};
use mirrord_config::agent::AgentConfig;
use mirrord_progress::Progress;
use tokio::pin;
use tracing::debug;

use super::util::agent_env;
use crate::{
    api::{
        container::{
            util::{base_command_line, get_capabilities, wait_for_agent_startup},
            ContainerParams, ContainerVariant,
        },
        kubernetes::AgentKubernetesConnectInfo,
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
    runtime_data: &RuntimeData,
    variant: &V,
    progress: &P,
) -> Result<AgentKubernetesConnectInfo>
where
    P: Progress + Send + Sync,
    V: ContainerVariant<Update = KubeEphemeralContainer>,
{
    let params = variant.params();
    // Ephemeral should never be targetless, so there should be runtime data.
    let mut container_progress = progress.subtask("creating ephemeral container...");

    let mut ephemeral_container: KubeEphemeralContainer = variant.as_update();
    debug!("Requesting ephemeral_containers_subresource");

    let pod_api = Api::namespaced(client.clone(), &runtime_data.pod_namespace);
    let pod: Pod = pod_api.get(&runtime_data.pod_name).await?;
    let container_spec = pod
        .spec
        .as_ref()
        .ok_or_else(|| KubeApiError::missing_field(&pod, "spec"))?
        .containers
        .iter()
        .find(|c| c.name == runtime_data.container_name)
        .ok_or_else(|| KubeApiError::invalid_state(&pod, runtime_data.container_name.clone()))?;

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

    let spec = ephemeral_containers_subresource
        .spec
        .as_mut()
        .ok_or_else(|| KubeApiError::missing_field(&pod, "spec"))?;

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
            serde_json::to_vec(&ephemeral_containers_subresource)
                .expect("serialization to vector never fails"),
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
    match version.as_ref() {
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
        pod_namespace: runtime_data.pod_namespace.clone(),
        agent_port: params.port,
    })
}

pub struct EphemeralTargetedVariant<'c> {
    agent: &'c AgentConfig,
    command_line: Vec<String>,
    params: &'c ContainerParams,
    runtime_data: &'c RuntimeData,
}

impl<'c> EphemeralTargetedVariant<'c> {
    pub fn new(
        agent: &'c AgentConfig,
        params: &'c ContainerParams,
        runtime_data: &'c RuntimeData,
    ) -> Self {
        let mut command_line = base_command_line(agent, params);

        command_line.extend(["ephemeral".to_string()]);

        EphemeralTargetedVariant {
            agent,
            params,
            command_line,
            runtime_data,
        }
    }
}

impl ContainerVariant for EphemeralTargetedVariant<'_> {
    type Update = KubeEphemeralContainer;

    fn agent_config(&self) -> &AgentConfig {
        self.agent
    }

    fn params(&self) -> &ContainerParams {
        self.params
    }

    fn as_update(&self) -> KubeEphemeralContainer {
        let EphemeralTargetedVariant {
            agent,
            params,
            runtime_data,
            command_line,
        } = self;
        let mut env = agent_env(agent, params);

        if let Some(mesh_vendor) = self.runtime_data.mesh.as_ref() {
            env.push(envs::IN_SERVICE_MESH.as_k8s_spec(&true));
            if matches!(mesh_vendor, MeshVendor::IstioCni) {
                env.push(envs::ISTIO_CNI.as_k8s_spec(&true));
            }
        };

        // Ephemeral only cares about `container_id` when `shareProcessNamespace` is set.
        // We need this to find the right `pid` for file ops in the target.
        if runtime_data.share_process_namespace {
            env.push(envs::EPHEMERAL_TARGET_CONTAINER_ID.as_k8s_spec(&runtime_data.container_id));
        }

        KubeEphemeralContainer {
            name: params.name.clone(),
            image: Some(agent.image().to_string()),
            security_context: Some(SecurityContext {
                run_as_group: Some(params.gid.into()),
                capabilities: Some(Capabilities {
                    add: Some(
                        get_capabilities(agent)
                            .iter()
                            .map(ToString::to_string)
                            .collect(),
                    ),
                    ..Default::default()
                }),
                privileged: Some(agent.privileged),
                run_as_non_root: agent.privileged.then_some(false),
                run_as_user: agent.privileged.then_some(0),
                ..Default::default()
            }),
            image_pull_policy: Some(agent.image_pull_policy.clone()),
            target_container_name: Some(runtime_data.container_name.clone()),
            env: Some(env),
            command: Some(command_line.clone()),
            ..Default::default()
        }
    }
}
