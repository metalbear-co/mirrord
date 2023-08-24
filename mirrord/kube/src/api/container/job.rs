use futures::StreamExt;
use k8s_openapi::api::{batch::v1::Job, core::v1::Pod};
use kube::{
    api::{ListParams, PostParams},
    runtime::{watcher, WatchStreamExt},
    Api, Client,
};
use mirrord_config::agent::AgentConfig;
use mirrord_progress::Progress;
use serde_json::json;
use tokio::pin;
use tracing::debug;

use crate::{
    api::{
        container::{
            util::{
                get_agent_image, get_agent_name, get_capabilities, wait_for_agent_startup,
                DEFAULT_TOLERATIONS,
            },
            ContainerApi,
        },
        kubernetes::{get_k8s_resource_api, AgentKubernetesConnectInfo},
        runtime::{NodeCheck, RuntimeData},
    },
    error::{KubeApiError, Result},
};

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
    ) -> Result<AgentKubernetesConnectInfo>
    where
        P: Progress + Send + Sync,
    {
        if agent.check_out_of_pods && let Some(runtime_data) = runtime_data.as_ref() {
            let mut check_node = progress.subtask("checking if node is allocatable...");
            match runtime_data.check_node(client).await {
                NodeCheck::Success => check_node.success(Some("node is allocatable")),
                NodeCheck::Error(err) => {
                    debug!("{err}");
                    check_node.warning("unable to check if node is allocatable");
                },
                NodeCheck::Failed(node_name, pods) => {
                    check_node.failure(Some("node is not allocatable"));

                    return Err(KubeApiError::NodePodLimitExceeded(node_name, pods));
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
                                "image": get_agent_image(agent),
                                "imagePullPolicy": agent.image_pull_policy,
                                "securityContext": targeted.then(||
                                    json!({
                                        "runAsGroup": agent_gid,
                                        "privileged": agent.privileged,
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

        Ok(AgentKubernetesConnectInfo {
            pod_name,
            agent_port: connection_port,
            namespace: agent.namespace.clone(),
        })
    }
}
