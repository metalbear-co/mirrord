use std::{collections::HashSet, sync::LazyLock};

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::{
    batch::v1::Job,
    core::v1::{ContainerStatus, Pod},
};
use kube::{
    api::{ListParams, LogParams, PostParams},
    runtime::{watcher, WatchStreamExt},
    Api, Client,
};
use mirrord_config::agent::AgentConfig;
use mirrord_progress::TaskProgress;
use rand::distributions::{Alphanumeric, DistString};
use serde_json::json;
use tokio::pin;
use tracing::debug;

use crate::{
    api::{get_k8s_api, runtime::RuntimeData},
    error::{KubeApiError, Result},
};

#[async_trait]
pub trait ContainerApi {
    async fn create_agent(
        client: &Client,
        agent: &AgentConfig,
        runtime_data: RuntimeData,
        agent_image: String,
        connection_port: u16,
        progress: &TaskProgress,
    ) -> Result<String>;
}

pub const SKIP_NAMES: LazyLock<HashSet<&'static str>> =
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
    async fn create_agent(
        client: &Client,
        agent: &AgentConfig,
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
        if let Some(timeout) = agent.communication_timeout {
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
                    "ttlSecondsAfterFinished": agent.ttl,

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
                                "imagePullPolicy": agent.image_pull_policy,
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
                                "env": [{"name": "RUST_LOG", "value": agent.log_level}],
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
        let job_api = get_k8s_api(client, agent.namespace.as_deref());

        job_api
            .create(&PostParams::default(), &agent_pod)
            .await
            .map_err(KubeApiError::KubeError)?;

        let params = ListParams::default()
            .labels(&format!("job-name={}", mirrord_agent_job_name))
            .timeout(60);

        pod_progress.done_with("agent pod created");

        let pod_progress = progress.subtask("waiting for pod to be ready...");

        let pod_api: Api<Pod> = get_k8s_api(client, agent.namespace.as_deref());

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

// #[async_trait]
// impl ContainerApi for EphemeralContainer {
//     async fn create_agent() -> Result<String> {
//         todo!()
//     }
// }
