use std::{collections::HashSet, sync::LazyLock};

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::{
    batch::v1::Job,
    core::v1::{ContainerStatus, EphemeralContainer as KubeEphemeralContainer, Pod},
};
use kube::{
    api::{ListParams, LogParams, PostParams},
    runtime::{watcher, WatchStreamExt},
    Api, Client,
};
use mirrord_config::agent::AgentConfig;
use mirrord_progress::Progress;
use rand::distributions::{Alphanumeric, DistString};
use serde_json::json;
use tokio::pin;
use tracing::{debug, warn};

use crate::{
    api::{get_k8s_resource_api, runtime::RuntimeData},
    error::{KubeApiError, Result},
};

#[async_trait]
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
        runtime_data: RuntimeData,
        connection_port: u16,
        progress: &P,
        agent_gid: u16,
    ) -> Result<String>
    where
        P: Progress + Send + Sync;
}

pub static SKIP_NAMES: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| HashSet::from(["istio-proxy", "linkerd-proxy", "proxy-init", "istio-init"]));

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

#[derive(Debug)]
pub struct JobContainer;

#[async_trait]
impl ContainerApi for JobContainer {
    async fn create_agent<P>(
        client: &Client,
        agent: &AgentConfig,
        runtime_data: RuntimeData,
        connection_port: u16,
        progress: &P,
        agent_gid: u16,
    ) -> Result<String>
    where
        P: Progress + Send + Sync,
    {
        let agent_image = Self::agent_image(agent);
        let pod_progress = progress.subtask("creating agent pod...");
        let mirrord_agent_job_name = get_agent_name();

        let mut agent_command_line = vec![
            "./mirrord-agent".to_owned(),
            "--container-id".to_owned(),
            runtime_data.container_id,
            "--container-runtime".to_owned(),
            runtime_data.container_runtime.to_string(),
            "-l".to_owned(),
            connection_port.to_string(),
        ];
        if let Some(timeout) = agent.communication_timeout {
            agent_command_line.push("-t".to_owned());
            agent_command_line.push(timeout.to_string());
        }
        if agent.pause {
            agent_command_line.push("--pause".to_owned());
        }

        #[cfg(debug_assertions)]
        if agent.test_error {
            agent_command_line.push("--test-error".to_owned());
        }

        let flush_connections = agent.flush_connections.to_string();

        let agent_pod: Job = serde_json::from_value(
            json!({ // Only Jobs support self deletion after completion
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
                                "annotations":
                                {
                                    "sidecar.istio.io/inject": "false",
                                    "linkerd.io/inject": "disabled"
                                },
                                "labels": {
                                    "app": "mirrord"
                                }
                            },

                    "spec": {
                        "hostPID": true,
                        "nodeName": runtime_data.node_name,
                        "restartPolicy": "Never",
                        "volumes": [
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
                        ],
                        "containers": [
                            {
                                "name": "mirrord-agent",
                                "image": agent_image,
                                "imagePullPolicy": agent.image_pull_policy,
                                "securityContext": {
                                    "runAsGroup": agent_gid,
                                    "privileged": true,
                                },
                                "volumeMounts": [
                                    {
                                        "mountPath": "/host/run",
                                        "name": "hostrun"
                                    },
                                    {
                                        "mountPath": "/host/var",
                                        "name": "hostvar"
                                    }
                                ],
                                "command": agent_command_line,
                                "env": [
                                    { "name": "RUST_LOG", "value": agent.log_level },
                                    { "name": "MIRRORD_AGENT_STEALER_FLUSH_CONNECTIONS", "value": flush_connections }
                                ],
                                "resources": // Add requests to avoid getting defaulted https://github.com/metalbear-co/mirrord/issues/579
                                {
                                    "requests":
                                    {
                                        "cpu": "1m",
                                        "memory": "1Mi"
                                    }
                                }
                            }
                        ]
                    }
                }
            }
                }
            ),
        )?;
        let job_api = get_k8s_resource_api(client, agent.namespace.as_deref());

        job_api
            .create(&PostParams::default(), &agent_pod)
            .await
            .map_err(KubeApiError::KubeError)?;

        let params = ListParams::default()
            .labels(&format!("job-name={mirrord_agent_job_name}"))
            .timeout(60);

        pod_progress.done_with("agent pod created");

        let pod_progress = progress.subtask("waiting for pod to be ready...");

        let pod_api: Api<Pod> = get_k8s_resource_api(client, agent.namespace.as_deref());

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
            .list(&ListParams::default().labels(&format!("job-name={mirrord_agent_job_name}")))
            .await
            .map_err(KubeApiError::KubeError)?;

        let pod_name = pods
            .items
            .first()
            .and_then(|pod| pod.metadata.name.clone())
            .ok_or(KubeApiError::JobPodNotFound(mirrord_agent_job_name))?;

        wait_for_agent_startup(&pod_api, &pod_name, "mirrord-agent".to_string()).await?;

        pod_progress.done_with("pod is ready");

        Ok(pod_name)
    }
}

#[derive(Debug)]
pub struct EphemeralContainer;

#[async_trait]
impl ContainerApi for EphemeralContainer {
    async fn create_agent<P>(
        client: &Client,
        agent: &AgentConfig,
        runtime_data: RuntimeData,
        connection_port: u16,
        progress: &P,
        agent_gid: u16,
    ) -> Result<String>
    where
        P: Progress + Send + Sync,
    {
        let agent_image = Self::agent_image(agent);
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
        if let Some(timeout) = agent.communication_timeout {
            agent_command_line.push("-t".to_string());
            agent_command_line.push(timeout.to_string());
        }

        let flush_connections = agent.flush_connections.to_string();

        let ephemeral_container: KubeEphemeralContainer = serde_json::from_value(json!({
            "name": mirrord_agent_name,
            "image": agent_image,
            "securityContext": {
                "runAsGroup": agent_gid,
                "capabilities": {
                    "add": ["NET_RAW", "NET_ADMIN"],
                },
                "privileged": true,
            },
            "imagePullPolicy": agent.image_pull_policy,
            "targetContainerName": runtime_data.container_name,
            "env": [
                {"name": "RUST_LOG", "value": agent.log_level},
                { "name": "MIRRORD_AGENT_STEALER_FLUSH_CONNECTIONS", "value": flush_connections }
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
}
