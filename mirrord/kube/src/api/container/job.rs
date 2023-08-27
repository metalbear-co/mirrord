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
                get_agent_image, get_agent_name, get_capabilities, wait_for_agent_startup,
                DEFAULT_TOLERATIONS,
            },
            ContainerApi, ContainerUpdateParams, ContainerUpdateVariant, ContainerUpdater,
        },
        kubernetes::{get_k8s_resource_api, AgentKubernetesConnectInfo},
        runtime::{NodeCheck, RuntimeData},
    },
    error::{KubeApiError, Result},
};

impl ContainerUpdateParams {
    pub fn job(self) -> JobContainer<Box<dyn ContainerUpdater<Update = Job>>> {
        JobContainer::new(match self.variant {
            ContainerUpdateVariant::Targetless => {
                Box::new(JobUpdate::new(&self.name, self.connection_port))
            }
            ContainerUpdateVariant::Target { gid, runtime_data } => Box::new(
                TargetedJobUpdate::new(&self.name, self.connection_port, gid, runtime_data),
            ),
        })
    }
}

#[derive(Debug)]
pub struct JobContainer<U> {
    updater: U,
}

impl<U> JobContainer<U> {
    fn new(updater: U) -> Self {
        JobContainer { updater }
    }
}

impl<U: ?Sized> ContainerApi for JobContainer<Box<U>>
where
    U: ContainerUpdater<Update = Job>,
{
    /// runtime_data is `None` when targetless.
    async fn create_agent<P>(
        &self,
        client: &Client,
        agent: &AgentConfig,
        progress: &P,
    ) -> Result<AgentKubernetesConnectInfo>
    where
        P: Progress + Send + Sync,
    {
        if agent.check_out_of_pods && let Some(runtime_data) = self.updater.runtime_data() {
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

        let agent_pod: Job = self.updater.as_update(agent)?;

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
            agent_port: self.updater.connection_port(),
            namespace: agent.namespace.clone(),
        })
    }
}

pub struct JobUpdate {
    agent_port: u16,
    command_line: Vec<String>,
    job_name: String,
}

impl JobUpdate {
    fn new(job_name: &str, agent_port: u16) -> Self {
        let command_line = vec![
            "./mirrord-agent".to_owned(),
            "-l".to_owned(),
            agent_port.to_string(),
        ];

        JobUpdate {
            agent_port,
            command_line,
            job_name: job_name.to_string(),
        }
    }
}

impl ContainerUpdater for JobUpdate {
    type Update = Job;

    fn name(&self) -> &str {
        &self.job_name
    }

    fn connection_port(&self) -> u16 {
        self.agent_port
    }

    fn runtime_data(&self) -> Option<&RuntimeData> {
        None
    }

    fn as_update(&self, agent: &AgentConfig) -> Result<Self::Update> {
        let JobUpdate {
            command_line,
            job_name,
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

        let tolerations = agent.tolerations.as_ref().unwrap_or(&DEFAULT_TOLERATIONS);

        // Only Jobs support self deletion after completion
        serde_json::from_value(json!({
            "metadata": {
                "name": job_name,
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

pub struct TargetedJobUpdate {
    inner: JobUpdate,

    gid: u16,
    runtime_data: RuntimeData,
}

impl TargetedJobUpdate {
    fn new(job_name: &str, connection_port: u16, gid: u16, runtime_data: RuntimeData) -> Self {
        let mut inner = JobUpdate::new(job_name, connection_port);

        inner.command_line.extend([
            "--container-id".to_owned(),
            runtime_data.container_id.to_string(),
            "--container-runtime".to_owned(),
            runtime_data.container_runtime.to_string(),
        ]);

        TargetedJobUpdate {
            inner,
            gid,
            runtime_data,
        }
    }
}

impl ContainerUpdater for TargetedJobUpdate {
    type Update = Job;

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn connection_port(&self) -> u16 {
        self.inner.connection_port()
    }

    fn runtime_data(&self) -> Option<&RuntimeData> {
        Some(&self.runtime_data)
    }

    fn as_update(&self, agent: &AgentConfig) -> Result<Self::Update> {
        let TargetedJobUpdate {
            inner,
            gid,
            runtime_data,
        } = self;

        let update = serde_json::from_value(json!({
            "metadata": {
                "name": inner.job_name,
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
                                    "runAsGroup": gid,
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

        let mut job = inner.as_update(agent)?;
        job.merge_from(update);
        Ok(job)
    }
}

#[cfg(test)]
mod test {

    use mirrord_config::{agent::AgentFileConfig, config::MirrordConfig};

    use super::*;
    use crate::api::runtime::ContainerRuntime;

    #[test]
    fn targetless() -> Result<(), Box<dyn std::error::Error>> {
        let agent = AgentFileConfig::default().generate_config()?;

        let update = ContainerUpdateParams::targetless("foobar".to_string(), 3000)
            .job()
            .updater
            .as_update(&agent)?;

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
                                "command": ["./mirrord-agent", "-l", "3000"],
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
        let agent = AgentFileConfig::default().generate_config()?;

        let update = TargetedJobUpdate::new(
            "foobar",
            3000,
            13,
            RuntimeData {
                pod_name: "pod".to_string(),
                pod_namespace: None,
                node_name: "foobaz".to_string(),
                container_id: "container".to_string(),
                container_runtime: ContainerRuntime::Docker,
                container_name: "foo".to_string(),
            },
        )
        .as_update(&agent)?;

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
                                "command": ["./mirrord-agent", "-l", "3000", "--container-id", "container", "--container-runtime", "docker"],
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
