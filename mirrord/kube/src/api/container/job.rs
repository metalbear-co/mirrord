use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use futures::StreamExt;
use k8s_openapi::api::{
    batch::v1::{Job, JobSpec},
    core::v1::{Pod, PodStatus, PodTemplateSpec},
};
use kube::{
    Api, Client, ResourceExt,
    api::{ObjectMeta, PostParams},
    runtime::{
        WatchStreamExt,
        watcher::{self, Event, watcher},
    },
};
use mirrord_config::agent::AgentConfig;
use mirrord_progress::Progress;
use tokio::{pin, time::interval};

use crate::{
    api::{
        container::{
            ContainerParams, ContainerVariant,
            pod::{PodTargetedVariant, PodVariant},
            util::wait_for_agent_startup,
        },
        kubernetes::{AgentKubernetesConnectInfo, get_k8s_resource_api},
        runtime::RuntimeData,
    },
    error::{KubeApiError, Result},
    extract::{
        FromResource,
        metadata::{Name, Namespace},
    },
};

pub async fn create_job_agent<P, V>(
    client: &Client,
    variant: &V,
    progress: &P,
) -> Result<AgentKubernetesConnectInfo>
where
    P: Progress,
    V: ContainerVariant<Update = Job>,
{
    let params = variant.params();
    let mut pod_progress = progress.subtask("creating agent pod...");

    let agent = variant.agent_config();
    let agent_job: Job = variant.as_update();

    let job_api = get_k8s_resource_api(client, agent.namespace.as_deref());

    job_api
        .create(&PostParams::default(), &agent_job)
        .await
        .map_err(KubeApiError::KubeError)?;

    let watcher_config = watcher::Config::default()
        .labels(&format!("job-name={}", params.name))
        .timeout(60);

    let pod_api: Api<Pod> = get_k8s_resource_api(client, agent.namespace.as_deref());

    let stream = watcher(pod_api.clone(), watcher_config);
    pin!(stream);

    let agent_pod = stream
        .as_mut()
        .applied_objects()
        .next()
        .await
        .ok_or_else(|| {
            KubeApiError::AgentPodStartError("watch stream unexpectedly finished".to_owned())
        })?
        .map_err(|err| KubeApiError::AgentPodStartError(format!("watch stream failed: {err}")))?;

    let (Name(pod_name), Namespace(pod_namespace)) = FromResource::from_resource(&agent_pod, &())?;

    pod_progress.success(Some(&format!(
        "agent pod {pod_namespace}/{pod_name} created"
    )));

    let mut pod_progress = progress.subtask("waiting for pod to be ready...");

    let initialization_start = Instant::now();

    let mut long_initialization_timer = interval(Duration::from_secs(20));
    // First tick is instant
    long_initialization_timer.tick().await;

    let mut last_known_container_state = find_agent_container_state(&agent_pod.status);

    loop {
        tokio::select! {
            _ = long_initialization_timer.tick() => {
                pod_progress.warning(&format!(
                    "agent pod startup is taking over {}s, container state: {}",
                    initialization_start.elapsed().as_secs(),
                    last_known_container_state
                ));
            }
            event = stream.next() => {
                match event {
                    Some(Ok(Event::Apply(pod) | Event::InitApply(pod))) => {
                        last_known_container_state = find_agent_container_state(&pod.status);

                        let Some(ref status) = pod.status else {
                            continue;
                        };

                        let Some(ref phase) = status.phase else {
                            continue;
                        };

                        // Ref: https://kubernetes.io/docs/concepts/workloads/pods/pod-lifecycle/#pod-phase
                        match phase.as_str() {
                            "Pending" | "Unknown" => continue,
                            "Running" => break,
                            "Failed" => {
                                let message = format!(
                                    "agent pod failed ({}): {}",
                                    status.reason.as_deref().unwrap_or("<unknown reason>"),
                                    status.message.as_deref().unwrap_or("<no message>"),
                                );
                                pod_progress.failure(Some(&message));
                                return Err(KubeApiError::AgentPodStartError(message));
                            }
                            "Succeeded" => {
                                pod_progress.failure(Some("agent pod unexpectedly finished"));
                                return Err(KubeApiError::AgentPodStartError("agent pod unexpectedly finished".to_owned()));
                            }
                            phase => {
                                let message = format!("agent pod moved to an unexpected phase '{phase}'");
                                pod_progress.failure(Some(&message));
                                return Err(KubeApiError::AgentPodStartError(message));
                            },
                        }
                    }

                    Some(Ok(Event::Delete(_))) => {
                        pod_progress.failure(Some("agent pod was unexpectedly deleted"));
                        return Err(KubeApiError::AgentPodDeleted);
                    }

                    Some(Ok(Event::Init | Event::InitDone)) => continue,

                    Some(Err(error)) => {
                            pod_progress.failure(Some("watch stream failed"));
                            return Err(KubeApiError::AgentPodStartError(format!("watch stream failed: {error}")));
                    }

                    None => {
                            let message = "watch stream unexpectedly finished".to_owned();
                            pod_progress.failure(Some(&message));
                            return Err(KubeApiError::AgentPodStartError(message));
                    }
                }
            }
        }
    }

    let version = wait_for_agent_startup(&pod_api, &pod_name, "mirrord-agent".to_string()).await?;
    match version.as_ref() {
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
        pod_name: pod_name.to_owned(),
        pod_namespace: pod_namespace.to_owned(),
        agent_port: params.port,
    })
}

/// Tries finding mirrord-agent's container in `status.container_statuses` and returns a string
/// representing it's state.
fn find_agent_container_state(status: &Option<PodStatus>) -> String {
    status
        .as_ref()
        .into_iter()
        .flat_map(|status| status.container_statuses.as_deref().unwrap_or_default())
        .find(|status| status.name == "mirrord-agent")
        .and_then(|status| status.state.as_ref())
        .and_then(|state| {
            if state.running.is_some() {
                Some("running".into())
            } else if let Some(terminated) = &state.terminated {
                Some(format!(
                    "terminated ({})",
                    terminated.reason.as_deref().unwrap_or("no reason found")
                ))
            } else {
                state.waiting.as_ref().map(|waiting| {
                    format!(
                        "waiting ({})",
                        waiting.reason.as_deref().unwrap_or("no reason found")
                    )
                })
            }
        })
        .unwrap_or_else(|| "not found".into())
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

    fn as_update(&self) -> Self::Update {
        let config = self.agent_config();
        let params = self.params();

        let mut pod = self.inner.as_update();

        let mut labels = config
            .labels
            .clone()
            .map(BTreeMap::from_iter)
            .unwrap_or_default();

        labels.extend(BTreeMap::from([
            (
                "kuma.io/sidecar-injection".to_string(),
                "disabled".to_string(),
            ),
            ("app".to_string(), "mirrord".to_string()),
        ]));

        let mut annotations = config
            .annotations
            .clone()
            .map(BTreeMap::from_iter)
            .unwrap_or_default();

        annotations.extend(BTreeMap::from([
            ("sidecar.istio.io/inject".to_string(), "false".to_string()),
            ("linkerd.io/inject".to_string(), "disabled".to_string()),
        ]));

        pod.labels_mut().extend(labels.clone());
        pod.annotations_mut().extend(annotations.clone());

        Job {
            metadata: ObjectMeta {
                name: Some(params.name.clone()),
                labels: Some(labels),
                annotations: Some(annotations),
                ..Default::default()
            },
            spec: Some(JobSpec {
                ttl_seconds_after_finished: Some(config.ttl.into()),
                backoff_limit: Some(0),
                template: PodTemplateSpec {
                    metadata: Some(pod.metadata),
                    spec: pod.spec,
                },
                ..Default::default()
            }),
            ..Default::default()
        }
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

    fn as_update(&self) -> Job {
        self.inner.as_update()
    }
}

#[cfg(test)]
mod test {

    use mirrord_agent_env::envs;
    use mirrord_config::{
        agent::AgentFileConfig,
        config::{ConfigContext, MirrordConfig},
    };

    use super::*;
    use crate::api::{
        container::util::{DEFAULT_TOLERATIONS, get_capabilities},
        runtime::ContainerRuntime,
    };

    #[test]
    fn targetless() -> Result<(), Box<dyn std::error::Error>> {
        let mut config_context = ConfigContext::default();
        let agent = AgentFileConfig::default().generate_config(&mut config_context)?;
        let support_ipv6 = false;
        let params = ContainerParams {
            name: "foobar".to_string(),
            port: 3000,
            gid: 13,
            tls_cert: None,
            pod_ips: None,
            support_ipv6,
            steal_tls_config: Default::default(),
            idle_ttl: Default::default(),
        };

        let update = JobVariant::new(&agent, &params).as_update();

        let expected: Job = serde_json::from_value(serde_json::json!({
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
                "backoffLimit": 0,
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
                        "nodeSelector": {},
                        "serviceAccountName": agent.service_account,
                        "containers": [
                            {
                                "name": "mirrord-agent",
                                "image": agent.image(),
                                "imagePullPolicy": agent.image_pull_policy,
                                "command": ["./mirrord-agent", "-l", "3000", "targetless"],
                                "env": [
                                    { "name": envs::LOG_LEVEL.name, "value": agent.log_level },
                                    { "name": envs::STEALER_FLUSH_CONNECTIONS.name, "value": agent.flush_connections.to_string() },
                                    { "name": envs::JSON_LOG.name, "value": Some(agent.json_log.to_string()) },
                                    { "name": envs::IPV6_SUPPORT.name, "value": Some(support_ipv6.to_string()) },
                                    { "name": envs::PASSTHROUGH_MIRRORING.name, "value": "true" },
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
        let mut agent = AgentFileConfig::default().generate_config(&mut config_context)?;
        agent.nftables = Some(true);
        let support_ipv6 = false;
        let params = ContainerParams {
            name: "foobar".to_string(),
            port: 3000,
            gid: 13,
            tls_cert: None,
            pod_ips: None,
            support_ipv6,
            steal_tls_config: Default::default(),
            idle_ttl: Default::default(),
        };

        let update = JobTargetedVariant::new(
            &agent,
            &params,
            &RuntimeData {
                mesh: None,
                pod_name: "pod".to_string(),
                pod_ips: vec![],
                pod_namespace: "default".to_string(),
                node_name: "foobaz".to_string(),
                container_id: "container".to_string(),
                container_runtime: ContainerRuntime::Docker,
                container_name: "foo".to_string(),
                guessed_container: false,
                share_process_namespace: false,
                containers_probe_ports: vec![],
            },
        )
        .as_update();

        let expected: Job = serde_json::from_value(serde_json::json!({
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
                "backoffLimit": 0,
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
                        "nodeSelector": {},
                        "tolerations": *DEFAULT_TOLERATIONS,
                        "serviceAccountName": agent.service_account,
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
                                "command": ["./mirrord-agent", "-l", "3000", "targeted", "--container-id", "container", "--container-runtime", "docker"],
                                "env": [
                                    { "name": envs::LOG_LEVEL.name, "value": agent.log_level },
                                    { "name": envs::STEALER_FLUSH_CONNECTIONS.name, "value": agent.flush_connections.to_string() },
                                    { "name": envs::JSON_LOG.name, "value": Some(agent.json_log.to_string()) },
                                    { "name": envs::IPV6_SUPPORT.name, "value": Some(support_ipv6.to_string()) },
                                    { "name": envs::PASSTHROUGH_MIRRORING.name, "value": "true" },
                                    { "name": envs::NFTABLES.name, "value": "true" },
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
