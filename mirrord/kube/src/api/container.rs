use std::{collections::HashSet, sync::LazyLock};

use futures::{AsyncBufReadExt, StreamExt, TryStreamExt};
use k8s_openapi::api::{
    batch::v1::Job,
    core::v1::{ContainerStatus, EphemeralContainer as KubeEphemeralContainer, Pod, Toleration},
};
use kube::{
    api::{ListParams, LogParams, PostParams},
    runtime::{watcher, WatchStreamExt},
    Api, Client,
};
use mirrord_config::agent::{AgentConfig, LinuxCapability};
use mirrord_progress::Progress;
use rand::distributions::{Alphanumeric, DistString};
use regex::Regex;
use serde_json::json;
use tokio::pin;
use tracing::{debug, warn};

use crate::{
    api::{
        get_k8s_resource_api,
        runtime::{NodeCheck, RuntimeData},
    },
    error::{KubeApiError, Result},
};

/// Retrieve a list of Linux capabilities for the agent container.
fn get_capabilities(config: &AgentConfig) -> Vec<LinuxCapability> {
    let disabled = config.disabled_capabilities.clone().unwrap_or_default();

    LinuxCapability::all()
        .iter()
        .copied()
        .filter(|c| !disabled.contains(c))
        .collect()
}

pub trait ContainerApi {
    fn agent_image(agent: &AgentConfig) -> String {
        match &agent.image {
            Some(image) => image.clone(),
            None => concat!("ghcr.io/metalbear-co/mirrord:", env!("CARGO_PKG_VERSION")).to_owned(),
        }
    }

    async fn create_agent<P>(
        client: &Client,
        agent: &AgentConfig,
        runtime_data: Option<RuntimeData>,
        connection_port: u16,
        progress: &P,
        agent_gid: u16,
    ) -> Result<String>
    where
        P: Progress + Send + Sync;
}

pub static SKIP_NAMES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "istio-proxy",
        "istio-init",
        "linkerd-proxy",
        "linkerd-init",
        "vault-agent",
        "vault-agent-init",
    ])
});

static DEFAULT_TOLERATIONS: LazyLock<Vec<Toleration>> = LazyLock::new(|| {
    vec![Toleration {
        operator: Some("Exists".to_owned()),
        ..Default::default()
    }]
});

/// Choose container logic:
/// 1. Try to find based on given name
/// 2. Try to find first container in pod that isn't a mesh side car
/// 3. Take first container in pod
pub fn choose_container<'a>(
    container_name: &Option<String>,
    container_statuses: &'a [ContainerStatus],
) -> Option<&'a ContainerStatus> {
    if let Some(name) = container_name {
        container_statuses
            .iter()
            .find(|&status| &status.name == name)
    } else {
        // Choose any container that isn't part of the skip list
        container_statuses
            .iter()
            .find(|&status| !SKIP_NAMES.contains(status.name.as_str()))
            .or_else(|| container_statuses.first())
    }
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

fn get_agent_name() -> String {
    let agent_name = format!(
        "mirrord-agent-{}",
        Alphanumeric
            .sample_string(&mut rand::thread_rng(), 10)
            .to_lowercase()
    );
    agent_name
}

static AGENT_READY_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("agent ready( - version (\\S+))?").expect("failed to create regex")
});

/**
 * Wait until the agent prints the "agent ready" message.
 * Return agent version extracted from the message (if found).
 */
async fn wait_for_agent_startup(
    pod_api: &Api<Pod>,
    pod_name: &str,
    container_name: String,
) -> Result<Option<String>> {
    let logs = pod_api
        .log_stream(
            pod_name,
            &LogParams {
                follow: true,
                container: Some(container_name),
                ..LogParams::default()
            },
        )
        .await?;

    let mut lines = logs.lines();
    while let Some(line) = lines.try_next().await? {
        let Some(captures) = AGENT_READY_REGEX.captures(&line) else {
            continue;
        };

        let version = captures.get(2).map(|m| m.as_str().to_string());
        return Ok(version);
    }

    Err(KubeApiError::AgentReadyMessageMissing)
}

#[derive(Debug)]
pub struct JobContainer;

impl ContainerApi for JobContainer {
    /// runtime_data is `None` when targetless.
    async fn create_agent<P>(
        client: &Client,
        agent: &AgentConfig,
        runtime_data: Option<RuntimeData>,
        connection_port: u16,
        progress: &P,
        agent_gid: u16,
    ) -> Result<String>
    where
        P: Progress + Send + Sync,
    {
        if agent.check_out_of_pods && let Some(runtime_data) = runtime_data.as_ref() {
            let mut check_node = progress.subtask("checking if node is allocatable...");
            match runtime_data.check_node(client).await {
                NodeCheck::Success(()) => check_node.success(Some("node is ok")),
                NodeCheck::Error(err) => {
                    debug!("{err}");

                    check_node.warning("unable to check if node is allocatable");
                },
                NodeCheck::Failed(err) => {
                    check_node.failure(Some("node is not allocatable"));
                    return Err(err);
                }
            }
        }

        let mut pod_progress = progress.subtask("creating agent pod...");
        let mirrord_agent_job_name = get_agent_name();

        let mut agent_command_line = vec![
            "./mirrord-agent".to_owned(),
            "-l".to_owned(),
            connection_port.to_string(),
        ];

        if let Some(runtime_data) = runtime_data.as_ref() {
            agent_command_line.push("--container-id".to_owned());
            agent_command_line.push(runtime_data.container_id.to_string());
            agent_command_line.push("--container-runtime".to_owned());
            agent_command_line.push(runtime_data.container_runtime.to_string());
        }

        if let Some(timeout) = agent.communication_timeout {
            agent_command_line.push("-t".to_owned());
            agent_command_line.push(timeout.to_string());
        }

        #[cfg(debug_assertions)]
        if agent.test_error {
            agent_command_line.push("--test-error".to_owned());
        }

        let tolerations = agent.tolerations.as_ref().unwrap_or(&DEFAULT_TOLERATIONS);

        let targeted = runtime_data.is_some();

        let json_value = json!({ // Only Jobs support self deletion after completion
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
                "ttlSecondsAfterFinished": agent.ttl,
                "template": {
                    "metadata": {
                        "annotations": {
                            "sidecar.istio.io/inject": "false",
                            "linkerd.io/inject": "disabled"
                        },
                        "labels": {
                            "app": "mirrord"
                        }
                    },

                    "spec": {
                        "hostPID": targeted,
                        "nodeName": runtime_data.map(|rd| rd.node_name),
                        "restartPolicy": "Never",
                        "volumes": targeted.then(|| json!(
                            [
                                {
                                    "name": "hostrun",
                                    "hostPath": {
                                        "path": "/run"
                                    }
                                },
                                {
                                    "name": "hostvar",
                                    "hostPath": {
                                        "path": "/var"
                                    }
                                }
                            ]
                        )),
                        "imagePullSecrets": agent.image_pull_secrets,
                        "tolerations": tolerations,
                        "containers": [
                            {
                                "name": "mirrord-agent",
                                "image": Self::agent_image(agent),
                                "imagePullPolicy": agent.image_pull_policy,
                                "securityContext": targeted.then(||
                                    json!({
                                        "runAsGroup": agent_gid,
                                        "capabilities": {
                                            "add": get_capabilities(agent),
                                        }
                                    })
                                ),
                                "volumeMounts": targeted.then(||
                                    json!([
                                        {
                                            "mountPath": "/host/run",
                                            "name": "hostrun"
                                        },
                                        {
                                            "mountPath": "/host/var",
                                            "name": "hostvar"
                                        }
                                    ])),
                                "command": agent_command_line,
                                "env": [
                                    { "name": "RUST_LOG", "value": agent.log_level },
                                    { "name": "MIRRORD_AGENT_STEALER_FLUSH_CONNECTIONS", "value": agent.flush_connections.to_string() }
                                ],
                                "resources": // Add requests to avoid getting defaulted https://github.com/metalbear-co/mirrord/issues/579
                                {
                                    "requests":
                                    {
                                        "cpu": "1m",
                                        "memory": "1Mi"
                                    },
                                    "limits":
                                    {
                                        "cpu": "100m",
                                        "memory": "100Mi"
                                    },
                                }
                            }
                        ]
                    }
                }
            }
        });

        let agent_pod: Job = serde_json::from_value(json_value)?;

        let job_api = get_k8s_resource_api(client, agent.namespace.as_deref());

        job_api
            .create(&PostParams::default(), &agent_pod)
            .await
            .map_err(KubeApiError::KubeError)?;

        let watcher_config = watcher::Config::default()
            .labels(&format!("job-name={mirrord_agent_job_name}"))
            .timeout(60);

        pod_progress.success(Some("agent pod created"));

        let mut pod_progress = progress.subtask("waiting for pod to be ready...");

        let pod_api: Api<Pod> = get_k8s_resource_api(client, agent.namespace.as_deref());

        let stream = watcher(pod_api.clone(), watcher_config).applied_objects();
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
            .list(&ListParams::default().labels(&format!("job-name={mirrord_agent_job_name}")))
            .await
            .map_err(KubeApiError::KubeError)?;

        let pod_name = pods
            .items
            .first()
            .and_then(|pod| pod.metadata.name.clone())
            .ok_or(KubeApiError::JobPodNotFound(mirrord_agent_job_name))?;

        let version =
            wait_for_agent_startup(&pod_api, &pod_name, "mirrord-agent".to_string()).await?;
        match version {
            Some(version) if version != env!("CARGO_PKG_VERSION") => {
                let message = format!(
                    "Agent version {version} does not match the local mirrord version {}. This may lead to unexpected errors.",
                    env!("CARGO_PKG_VERSION"),
                );
                pod_progress.warning(&message);
            }
            _ => {}
        }

        pod_progress.success(Some("pod is ready"));

        Ok(pod_name)
    }
}

#[derive(Debug)]
pub struct EphemeralContainer;

impl ContainerApi for EphemeralContainer {
    async fn create_agent<P>(
        client: &Client,
        agent: &AgentConfig,
        runtime_data: Option<RuntimeData>,
        connection_port: u16,
        progress: &P,
        agent_gid: u16,
    ) -> Result<String>
    where
        P: Progress + Send + Sync,
    {
        // Ephemeral should never be targetless, so there should be runtime data.
        let runtime_data = runtime_data.ok_or(KubeApiError::MissingRuntimeData)?;
        let mut container_progress = progress.subtask("creating ephemeral container...");

        warn!("Ephemeral Containers is an experimental feature
                  >> Refer https://kubernetes.io/docs/concepts/workloads/pods/ephemeral-containers/ for more info");

        let mirrord_agent_name = get_agent_name();

        let mut agent_command_line = vec![
            "./mirrord-agent".to_string(),
            "-l".to_string(),
            connection_port.to_string(),
            "-e".to_string(),
        ];
        if let Some(timeout) = agent.communication_timeout {
            agent_command_line.push("-t".to_string());
            agent_command_line.push(timeout.to_string());
        }

        let ephemeral_container: KubeEphemeralContainer = serde_json::from_value(json!({
            "name": mirrord_agent_name,
            "image": Self::agent_image(agent),
            "securityContext": {
                "runAsGroup": agent_gid,
                "capabilities": {
                    "add": get_capabilities(agent),
                },
            },
            "imagePullPolicy": agent.image_pull_policy,
            "targetContainerName": runtime_data.container_name,
            "env": [
                {"name": "RUST_LOG", "value": agent.log_level},
                { "name": "MIRRORD_AGENT_STEALER_FLUSH_CONNECTIONS", "value": agent.flush_connections.to_string() }
            ],
            "command": agent_command_line,
        }))?;
        debug!("Requesting ephemeral_containers_subresource");

        let pod_api = get_k8s_resource_api(client, agent.namespace.as_deref());
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
                serde_json::to_vec(&ephemeral_containers_subresource)
                    .map_err(KubeApiError::from)?,
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
            if is_ephemeral_container_running(pod, &mirrord_agent_name) {
                debug!("container ready");
                break;
            } else {
                debug!("container not ready yet");
            }
        }

        let version =
            wait_for_agent_startup(&pod_api, &runtime_data.pod_name, mirrord_agent_name).await?;
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
        Ok(runtime_data.pod_name.to_string())
    }
}

#[cfg(test)]
mod test {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("agent ready", None)]
    #[case("agent ready - version 3.56.0", Some("3.56.0"))]
    fn agent_version_regex(#[case] agent_message: &str, #[case] version: Option<&str>) {
        let captures = AGENT_READY_REGEX.captures(agent_message).unwrap();

        assert_eq!(captures.get(2).map(|c| c.as_str()), version);
    }
}
