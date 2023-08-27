use futures::StreamExt;
use k8s_openapi::api::{batch::v1::Job, core::v1::Pod};
use kube::{
    api::{ListParams, PostParams},
    runtime::{watcher, WatchStreamExt},
    Api, Client,
};
use mirrord_config::agent::AgentConfig;
use mirrord_progress::Progress;
use tokio::pin;
use tracing::debug;

use crate::{
    api::{
        container::{
            util::{wait_for_agent_startup, ContainerParams},
            ContainerVariant,
        },
        kubernetes::{get_k8s_resource_api, AgentKubernetesConnectInfo},
    },
    error::{KubeApiError, Result},
};

pub trait ContainerApi<U> {
    async fn create_agent<V, P>(
        &self,
        variant: V,
        progress: &P,
    ) -> Result<AgentKubernetesConnectInfo>
    where
        P: Progress + Send + Sync,
        V: ContainerVariant<Update = U>;
}

pub struct Targetless<'c> {
    agent: &'c AgentConfig,
    client: &'c Client,
    params: ContainerParams,
}

impl<'c> Targetless<'c> {
    pub fn new(client: &'c Client, agent: &'c AgentConfig) -> Self {
        Targetless {
            agent,
            client,
            params: ContainerParams::new(),
        }
    }
}

impl ContainerApi<Job> for Targetless<'_> {
    async fn create_agent<V, P>(
        &self,
        variant: V,
        progress: &P,
    ) -> Result<AgentKubernetesConnectInfo>
    where
        P: Progress + Send + Sync,
        V: ContainerVariant<Update = Job>,
    {
        let Targetless {
            agent,
            client,
            params,
        } = self;

        let mut pod_progress = progress.subtask("creating agent pod...");

        let agent_pod: Job = variant.as_update(agent)?;

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
            agent_port: params.port,
            namespace: agent.namespace.clone(),
        })
    }
}
