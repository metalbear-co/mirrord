use k8s_openapi::{api::core::v1::Pod, DeepMerge};
use mirrord_config::agent::AgentConfig;
use serde_json::json;

use super::util::agent_env;
use crate::{
    api::{
        container::{
            util::{base_command_line, get_capabilities, DEFAULT_TOLERATIONS},
            ContainerParams, ContainerVariant,
        },
        runtime::RuntimeData,
    },
    error::{KubeApiError, Result},
};

pub struct PodVariant<'c> {
    agent: &'c AgentConfig,
    command_line: Vec<String>,
    params: &'c ContainerParams,
}

impl<'c> PodVariant<'c> {
    pub fn new(agent: &'c AgentConfig, params: &'c ContainerParams) -> Self {
        let mut command_line = base_command_line(agent, params);

        command_line.push("targetless".to_owned());

        PodVariant::with_command_line(agent, params, command_line)
    }

    fn with_command_line(
        agent: &'c AgentConfig,
        params: &'c ContainerParams,
        command_line: Vec<String>,
    ) -> Self {
        PodVariant {
            agent,
            command_line,
            params,
        }
    }
}

impl ContainerVariant for PodVariant<'_> {
    type Update = Pod;

    fn agent_config(&self) -> &AgentConfig {
        self.agent
    }

    fn params(&self) -> &ContainerParams {
        self.params
    }

    fn as_update(&self) -> Result<Pod> {
        let PodVariant {
            agent,
            command_line,
            params,
            ..
        } = self;

        let tolerations = agent.tolerations.as_ref().unwrap_or(&DEFAULT_TOLERATIONS);

        let resources = agent.resources.clone().unwrap_or_else(|| {
            serde_json::from_value(serde_json::json!({
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
            }))
            .expect("Should be valid ResourceRequirements json")
        });

        let env = agent_env(agent, params);

        serde_json::from_value(json!({
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
                "tolerations": tolerations,
                "containers": [
                    {
                        "name": "mirrord-agent",
                        "image": agent.image(),
                        "imagePullPolicy": agent.image_pull_policy,
                        "command": command_line,
                        "env": env,
                        // Add requests to avoid getting defaulted https://github.com/metalbear-co/mirrord/issues/579
                        "resources": resources
                    }
                ]
            }
        }))
        .map_err(KubeApiError::from)
    }
}

pub struct PodTargetedVariant<'c> {
    inner: PodVariant<'c>,
    runtime_data: &'c RuntimeData,
}

impl<'c> PodTargetedVariant<'c> {
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

        if let Some(mesh) = runtime_data.mesh {
            command_line.extend(["--mesh".to_string(), mesh.to_string()]);
        }

        let inner = PodVariant::with_command_line(agent, params, command_line);

        PodTargetedVariant {
            inner,
            runtime_data,
        }
    }
}

impl ContainerVariant for PodTargetedVariant<'_> {
    type Update = Pod;

    fn agent_config(&self) -> &AgentConfig {
        self.inner.agent_config()
    }

    fn params(&self) -> &ContainerParams {
        self.inner.params()
    }

    fn as_update(&self) -> Result<Pod> {
        let PodTargetedVariant { runtime_data, .. } = self;

        let agent = self.agent_config();
        let params = self.params();

        let update = serde_json::from_value(json!({
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
        }))?;

        let mut pod = self.inner.as_update()?;
        pod.merge_from(update);
        Ok(pod)
    }
}
