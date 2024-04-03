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
            pod::{PodTargetedVariant, PodVariant},
            util::wait_for_agent_startup,
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
        if let Some(status) = &pod.status
            && let Some(phase) = &status.phase
        {
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

pub struct JobVariant<T> {
    inner: T,
}

impl<'c> JobVariant<PodVariant<'c>> {
    pub fn new(agent: &'c AgentConfig, params: &'c ContainerParams) -> Self {
        JobVariant {
            inner: PodVariant::new(agent, params),
        }
    }
}

impl<T> ContainerVariant for JobVariant<T>
where
    T: ContainerVariant<Update = Pod>,
{
    type Update = Job;

    fn agent_config(&self) -> &AgentConfig {
        self.inner.agent_config()
    }

    fn params(&self) -> &ContainerParams {
        self.inner.params()
    }

    fn as_update(&self) -> Result<Self::Update> {
        let agent = self.agent_config();
        let params = self.params();

        serde_json::from_value(json!({
            "metadata": {
                "name": params.name,
                "labels": {
                    "kuma.io/sidecar-injection": "disabled",
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
                "template": self.inner.as_update()?
            }
        }))
        .map_err(KubeApiError::from)
    }
}

pub struct JobTargetedVariant<'c> {
    inner: JobVariant<PodTargetedVariant<'c>>,
}

impl<'c> JobTargetedVariant<'c> {
    pub fn new(
        agent: &'c AgentConfig,
        params: &'c ContainerParams,
        runtime_data: &'c RuntimeData,
    ) -> Self {
        let inner = PodTargetedVariant::new(agent, params, runtime_data);

        JobTargetedVariant {
            inner: JobVariant { inner },
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
        self.inner.as_update()
    }
}

#[cfg(test)]
mod test {

    use mirrord_config::{
        agent::AgentFileConfig,
        config::{ConfigContext, MirrordConfig},
    };

    use super::*;
    use crate::api::{
        container::util::{get_capabilities, DEFAULT_TOLERATIONS},
        runtime::ContainerRuntime,
    };

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
                            "kuma.io/sidecar-injection": "disabled",
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
                                    "kuma.io/sidecar-injection": "disabled",
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
                                        "image": agent.image(),
                                        "imagePullPolicy": agent.image_pull_policy,
                                        "command": ["./mirrord-agent", "-l", "3000", "targetless"],
                                        "env": [
                                            { "name": "RUST_LOG", "value": agent.log_level },
                                            { "name": "MIRRORD_AGENT_STEALER_FLUSH_CONNECTIONS", "value":
        agent.flush_connections.to_string() },                                     { "name":
        "MIRRORD_AGENT_NFTABLES", "value": agent.nftables.to_string() }
        ],                                 "resources": // Add requests to avoid getting defaulted https://github.com/metalbear-co/mirrord/issues/579
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
                mesh: None,
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
                            "kuma.io/sidecar-injection": "disabled",
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
                                    "kuma.io/sidecar-injection": "disabled",
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
                                        "image": agent.image(),
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
                                        "command": ["./mirrord-agent", "-l", "3000", "targeted",
        "--container-id", "container", "--container-runtime", "docker"],
        "env": [                                     { "name": "RUST_LOG", "value": agent.log_level },
                                            { "name": "MIRRORD_AGENT_STEALER_FLUSH_CONNECTIONS", "value":
        agent.flush_connections.to_string() },                                     { "name":
        "MIRRORD_AGENT_NFTABLES", "value": agent.nftables.to_string() }
        ],                                 "resources": // Add requests to avoid getting defaulted https://github.com/metalbear-co/mirrord/issues/579
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
