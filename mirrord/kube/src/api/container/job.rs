use futures::StreamExt;
use k8s_openapi::{
    api::{batch::v1::Job, core::v1::Pod},
    DeepMerge,
};
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
                base_command_line, get_agent_image, get_capabilities, wait_for_agent_startup,
                DEFAULT_TOLERATIONS,
            },
            ContainerParams, ContainerVariant,
        },
        kubernetes::{get_k8s_resource_api, AgentKubernetesConnectInfo},
        runtime::RuntimeData,
    },
    error::{KubeApiError, Result},
};

pub async fn create_job_agent<P, V>(
    client: &Client,
    variant: &V,
    progress: &P,
) -> Result<AgentKubernetesConnectInfo>
where
    P: Progress + Send + Sync,
    V: ContainerVariant<Update = Job>,
{
    let params = variant.params();
    let mut pod_progress = progress.subtask("creating agent pod...");

    let agent = variant.agent_config();
    let agent_pod: Job = variant.as_update()?;

    let job_api = get_k8s_resource_api(client, agent.namespace.as_deref());

    job_api
        .create(&PostParams::default(), &agent_pod)
        .await
        .map_err(KubeApiError::KubeError)?;

    let watcher_config = watcher::Config::default()
        .labels(&format!("job-name={}", params.name))
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
        .list(&ListParams::default().labels(&format!("job-name={}", params.name)))
        .await
        .map_err(KubeApiError::KubeError)?;

    let pod_name = pods
        .items
        .first()
        .and_then(|pod| pod.metadata.name.clone())
        .ok_or(KubeApiError::JobPodNotFound(params.name.clone()))?;

    let version = wait_for_agent_startup(&pod_api, &pod_name, "mirrord-agent".to_string()).await?;
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
        agent_port: params.port,
        namespace: agent.namespace.clone(),
    })
}

pub struct JobVariant<'c> {
    agent: &'c AgentConfig,
    command_line: Vec<String>,
    params: &'c ContainerParams,
}

impl<'c> JobVariant<'c> {
    pub fn new(agent: &'c AgentConfig, params: &'c ContainerParams) -> Self {
        let mut command_line = base_command_line(agent, params);

        command_line.push("targetless".to_owned());

        JobVariant::with_command_line(agent, params, command_line)
    }

    fn with_command_line(
        agent: &'c AgentConfig,
        params: &'c ContainerParams,
        command_line: Vec<String>,
    ) -> Self {
        JobVariant {
            agent,
            command_line,
            params,
        }
    }
}

impl ContainerVariant for JobVariant<'_> {
    type Update = Job;

    fn agent_config(&self) -> &AgentConfig {
        self.agent
    }

    fn params(&self) -> &ContainerParams {
        self.params
    }

    fn as_update(&self) -> Result<Job> {
        let JobVariant {
            agent,
            command_line,
            params,
        } = self;

        let tolerations = agent.tolerations.as_ref().unwrap_or(&DEFAULT_TOLERATIONS);

        // Only Jobs support self deletion after completion
        serde_json::from_value(json!({
            "metadata": {
                "name": params.name,
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
                        "restartPolicy": "Never",
                        "imagePullSecrets": agent.image_pull_secrets,
                        "tolerations": tolerations,
                        "containers": [
                            {
                                "name": "mirrord-agent",
                                "image": get_agent_image(agent),
                                "imagePullPolicy": agent.image_pull_policy,
                                "command": command_line,
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
        })).map_err(KubeApiError::from)
    }
}

pub struct JobTargetedVariant<'c> {
    inner: JobVariant<'c>,
    runtime_data: &'c RuntimeData,
}

impl<'c> JobTargetedVariant<'c> {
    pub fn new(
        agent: &'c AgentConfig,
        params: &'c ContainerParams,
        runtime_data: &'c RuntimeData,
    ) -> Self {
        let mut command_line = base_command_line(agent, params);

        command_line.extend([
            "targeted".to_owned(),
            "--container-id".to_owned(),
            runtime_data.container_id.to_string(),
            "--container-runtime".to_owned(),
            runtime_data.container_runtime.to_string(),
        ]);

        let inner = JobVariant::with_command_line(agent, params, command_line);

        JobTargetedVariant {
            inner,
            runtime_data,
        }
    }
}

impl ContainerVariant for JobTargetedVariant<'_> {
    type Update = Job;

    fn agent_config(&self) -> &AgentConfig {
        self.inner.agent_config()
    }

    fn params(&self) -> &ContainerParams {
        self.inner.params()
    }

    fn as_update(&self) -> Result<Job> {
        let JobTargetedVariant {
            inner: JobVariant { agent, params, .. },
            runtime_data,
        } = self;

        let update = serde_json::from_value(json!({
            "metadata": {
                "name": params.name,
            },
            "spec": {
                "template": {
                    "spec": {
                        "hostPID": true,
                        "nodeName": runtime_data.node_name,
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
                                "securityContext": {
                                    "runAsGroup": params.gid,
                                    "privileged": agent.privileged,
                                    "capabilities": {
                                        "add": get_capabilities(agent),
                                    }
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
                            }
                        ]
                    }
                }
            }
        }))?;

        let mut job = self.inner.as_update()?;
        job.merge_from(update);
        Ok(job)
    }
}

#[cfg(test)]
mod test {

    use mirrord_config::{
        agent::AgentFileConfig,
        config::{ConfigContext, MirrordConfig},
    };

    use super::*;
    use crate::api::runtime::ContainerRuntime;

    #[test]
    fn targetless() -> Result<(), Box<dyn std::error::Error>> {
        let mut config_context = ConfigContext::default();
        let agent = AgentFileConfig::default().generate_config(&mut config_context)?;
        let params = ContainerParams {
            name: "foobar".to_string(),
            port: 3000,
            gid: 13,
        };

        let update = JobVariant::new(&agent, &params).as_update()?;

        let expected: Job = serde_json::from_value(json!({
            "metadata": {
                "name": "foobar",
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
                        "restartPolicy": "Never",
                        "imagePullSecrets": agent.image_pull_secrets,
                        "tolerations": *DEFAULT_TOLERATIONS,
                        "containers": [
                            {
                                "name": "mirrord-agent",
                                "image": get_agent_image(&agent),
                                "imagePullPolicy": agent.image_pull_policy,
                                "command": ["./mirrord-agent", "-l", "3000", "targetless"],
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
        }))?;

        assert_eq!(update, expected);

        Ok(())
    }

    #[test]
    fn targeted() -> Result<(), Box<dyn std::error::Error>> {
        let mut config_context = ConfigContext::default();
        let agent = AgentFileConfig::default().generate_config(&mut config_context)?;
        let params = ContainerParams {
            name: "foobar".to_string(),
            port: 3000,
            gid: 13,
        };

        let update = JobTargetedVariant::new(
            &agent,
            &params,
            &RuntimeData {
                is_mesh: false,
                pod_name: "pod".to_string(),
                pod_namespace: None,
                node_name: "foobaz".to_string(),
                container_id: "container".to_string(),
                container_runtime: ContainerRuntime::Docker,
                container_name: "foo".to_string(),
            },
        )
        .as_update()?;

        let expected: Job = serde_json::from_value(json!({
            "metadata": {
                "name": "foobar",
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
                        "hostPID": true,
                        "nodeName": "foobaz",
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
                        "imagePullSecrets": agent.image_pull_secrets,
                        "tolerations": *DEFAULT_TOLERATIONS,
                        "containers": [
                            {
                                "name": "mirrord-agent",
                                "image": get_agent_image(&agent),
                                "imagePullPolicy": agent.image_pull_policy,
                                "securityContext": {
                                    "runAsGroup": 13,
                                    "privileged": agent.privileged,
                                    "capabilities": {
                                        "add": get_capabilities(&agent),
                                    }
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
                                "command": ["./mirrord-agent", "-l", "3000", "targeted", "--container-id", "container", "--container-runtime", "docker"],
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
        }))?;

        assert_eq!(update, expected);

        Ok(())
    }
}
