use k8s_openapi::{
    api::core::v1::{
        Capabilities, Container, HostPathVolumeSource, LocalObjectReference, Pod, PodSpec,
        SecurityContext, Volume, VolumeMount,
    },
    DeepMerge,
};
use kube::api::ObjectMeta;
use mirrord_config::agent::AgentConfig;

use super::util::agent_env;
use crate::api::{
    container::{
        util::{base_command_line, get_capabilities, DEFAULT_TOLERATIONS},
        ContainerParams, ContainerVariant,
    },
    runtime::RuntimeData,
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

    fn as_update(&self) -> Pod {
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
        let image_pull_secrets = agent.image_pull_secrets.as_ref().map(|secrets| {
            secrets
                .iter()
                .map(|secret| LocalObjectReference {
                    name: Some(secret.name.to_string()),
                })
                .collect()
        });

        Pod {
            metadata: ObjectMeta {
                annotations: Some(
                    [
                        ("sidecar.istio.io/inject".to_string(), "false".to_string()),
                        ("linkerd.io/inject".to_string(), "disabled".to_string()),
                    ]
                    .into(),
                ),
                labels: Some(
                    [
                        (
                            "kuma.io/sidecar-injection".to_string(),
                            "disabled".to_string(),
                        ),
                        ("app".to_string(), "mirrord".to_string()),
                    ]
                    .into(),
                ),
                ..Default::default()
            },
            spec: Some(PodSpec {
                restart_policy: Some("Never".to_string()),
                image_pull_secrets,
                tolerations: Some(tolerations.clone()),
                containers: vec![Container {
                    name: "mirrord-agent".to_string(),
                    image: Some(agent.image().to_string()),
                    image_pull_policy: Some(agent.image_pull_policy.clone()),
                    command: Some(command_line.clone()),
                    env: Some(env),
                    // Add requests to avoid getting defaulted https://github.com/metalbear-co/mirrord/issues/579
                    resources: Some(resources),
                    security_context: Some(SecurityContext {
                        privileged: Some(agent.privileged),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        }
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

    fn as_update(&self) -> Pod {
        let PodTargetedVariant { runtime_data, .. } = self;

        let agent = self.agent_config();
        let params = self.params();

        let update = Pod {
            spec: Some(PodSpec {
                host_pid: Some(true),
                node_name: Some(runtime_data.node_name.clone()),
                volumes: Some(vec![
                    Volume {
                        name: "hostrun".to_string(),
                        host_path: Some(HostPathVolumeSource {
                            path: "/run".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    Volume {
                        name: "hostvar".to_string(),
                        host_path: Some(HostPathVolumeSource {
                            path: "/var".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ]),
                containers: vec![Container {
                    name: "mirrord-agent".to_string(),
                    security_context: Some(SecurityContext {
                        run_as_group: Some(params.gid.into()),
                        privileged: Some(agent.privileged),
                        capabilities: Some(Capabilities {
                            add: Some(
                                get_capabilities(agent)
                                    .iter()
                                    .map(ToString::to_string)
                                    .collect(),
                            ),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    volume_mounts: Some(vec![
                        VolumeMount {
                            mount_path: "/host/run".to_string(),
                            name: "hostrun".to_string(),
                            ..Default::default()
                        },
                        VolumeMount {
                            mount_path: "/host/var".to_string(),
                            name: "hostvar".to_string(),
                            ..Default::default()
                        },
                    ]),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };

        let mut pod = self.inner.as_update();
        pod.merge_from(update);
        pod
    }
}
