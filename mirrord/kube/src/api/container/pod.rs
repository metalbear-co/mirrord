use std::collections::BTreeMap;

use k8s_openapi::{
    DeepMerge,
    api::core::v1::{
        Capabilities, Container, HostPathVolumeSource, LocalObjectReference, Pod, PodSpec,
        SecurityContext, Volume, VolumeMount,
    },
};
use kube::api::ObjectMeta;
use mirrord_agent_env::{envs, mesh::MeshVendor};
use mirrord_config::agent::AgentConfig;

use super::util::agent_env;
use crate::api::{
    container::{
        ContainerParams, ContainerVariant,
        util::{DEFAULT_TOLERATIONS, base_command_line, get_capabilities},
    },
    runtime::RuntimeData,
};

/// The `targetless` agent variant is created by this, see its [`PodVariant::as_update`].
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
                    name: secret.name.to_string(),
                })
                .collect()
        });
        let node_selector = agent
            .node_selector
            .clone()
            .map(BTreeMap::from_iter)
            .unwrap_or_default();

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
                tolerations: agent.tolerations.clone(),
                node_selector: Some(node_selector),
                containers: vec![Container {
                    name: "mirrord-agent".to_string(),
                    image: Some(agent.image().to_string()),
                    image_pull_policy: Some(agent.image_pull_policy.clone()),
                    command: Some(command_line.clone()),
                    env: Some(env),
                    // Add requests to avoid getting defaulted https://github.com/metalbear-co/mirrord/issues/579
                    resources: Some(resources),
                    ..Default::default()
                }],
                security_context: agent.security_context.clone(),
                priority_class_name: agent.priority_class.clone(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

/// The `targeted` agent variant is created by this.
///
/// It builds on top of [`PodVariant`], merging spec, etc from there. See
/// [`PodTargetedVariant::as_update`].
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

        let tolerations = agent.tolerations.as_ref().unwrap_or(&DEFAULT_TOLERATIONS);

        let env = self.runtime_data.mesh.map(|mesh_vendor| {
            let mut env = vec![envs::IN_SERVICE_MESH.as_k8s_spec(&true)];
            if matches!(mesh_vendor, MeshVendor::IstioCni) {
                env.push(envs::ISTIO_CNI.as_k8s_spec(&true));
            }
            env
        });

        let update = Pod {
            spec: Some(PodSpec {
                restart_policy: Some("Never".to_string()),
                tolerations: Some(tolerations.clone()),
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
                    env,
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

        // Remove priority class from spec if it's targeted.
        pod.spec
            .as_mut()
            .map(|pod_spec| pod_spec.priority_class_name.take());

        pod
    }
}

#[cfg(test)]
mod test {
    use mirrord_config::{
        agent::AgentFileConfig,
        config::{ConfigContext, MirrordConfig},
    };

    use crate::api::{
        container::{
            ContainerParams, ContainerVariant,
            pod::{PodTargetedVariant, PodVariant},
        },
        runtime::{ContainerRuntime, RuntimeData},
    };

    #[test]
    fn agent_priority_class() -> Result<(), Box<dyn std::error::Error>> {
        let mut config_context = ConfigContext::default();
        let mut agent = AgentFileConfig::default().generate_config(&mut config_context)?;
        agent.priority_class = Some("test-priority-profile".to_string());
        let params = ContainerParams {
            name: "foobar".to_string(),
            port: 3000,
            gid: 13,
            tls_cert: None,
            pod_ips: None,
            support_ipv6: false,
            steal_tls_config: Default::default(),
            idle_ttl: Default::default(),
        };

        // targetless agent pod can be configured with priority class name.
        let update = PodVariant::new(&agent, &params).as_update();
        assert_eq!(
            update.spec.unwrap().priority_class_name.unwrap(),
            "test-priority-profile"
        );

        // cannot configure priority class for targeted.
        let update = PodTargetedVariant::new(
            &agent,
            &params,
            &RuntimeData {
                mesh: None,
                pod_name: "some-pod".to_string(),
                pod_ips: vec![],
                pod_namespace: "default".to_string(),
                node_name: "some-node".to_string(),
                container_id: "container".to_string(),
                container_runtime: ContainerRuntime::Docker,
                container_name: "some-container".to_string(),
                guessed_container: false,
                share_process_namespace: false,
                containers_probe_ports: vec![],
            },
        )
        .as_update();
        assert!(update.spec.unwrap().priority_class_name.is_none());
        Ok(())
    }
}
