use async_trait::async_trait;
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, Client, Config};
use mirrord_config::{agent::AgentConfig, target::TargetConfig};
use mirrord_progress::TaskProgress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use rand::Rng;
use tokio::sync::mpsc;
use tracing::info;

#[cfg(feature = "env_guard")]
use crate::api::env_guard::EnvVarGuard;
use crate::{
    api::{
        container::{ContainerApi, EphemeralContainer, JobContainer},
        get_k8s_api,
        runtime::RuntimeDataProvider,
        wrap_raw_connection, AgentManagment,
    },
    error::{KubeApiError, Result},
};

pub struct KubernetesAPI {
    client: Client,
    agent: AgentConfig,
    target: TargetConfig,
}

impl KubernetesAPI {
    pub async fn create(agent: AgentConfig, target: TargetConfig) -> Result<Self> {
        #[cfg(feature = "env_guard")]
        let _guard = EnvVarGuard::new();

        #[cfg_attr(not(feature = "env_guard"), allow(unused_mut))]
        let mut config = Config::infer().await?;

        #[cfg(feature = "env_guard")]
        _guard.prepare_config(&mut config);

        let client = Client::try_from(config).map_err(KubeApiError::from)?;

        Ok(KubernetesAPI {
            client,
            agent,
            target,
        })
    }
}

#[async_trait]
impl AgentManagment for KubernetesAPI {
    type AgentRef = (String, u16);
    type Err = KubeApiError;

    async fn create_connection(
        &self,
        (pod_agent_name, agent_port): Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        let pod_api: Api<Pod> = get_k8s_api(&self.client, self.agent.namespace.as_deref());
        let mut port_forwarder = pod_api.portforward(&pod_agent_name, &[agent_port]).await?;

        wrap_raw_connection(port_forwarder.take_stream(agent_port).unwrap())
    }

    async fn create_agent(&self, progress: &TaskProgress) -> Result<Self::AgentRef, Self::Err> {
        let runtime_data = self
            .target
            .path.as_ref().ok_or_else(|| KubeApiError::InvalidTarget(
                "No target specified. Please set the `MIRRORD_IMPERSONATED_TARGET` environment variable.".to_owned(),
            ))?
            .runtime_data(&self.client, self.target.namespace.as_deref())
            .await?;

        info!("No existing agent, spawning new one.");
        let agent_port: u16 = rand::thread_rng().gen_range(30000..=65535);
        info!("Using port `{agent_port:?}` for communication");

        let pod_agent_name = if self.agent.ephemeral {
            EphemeralContainer::create_agent(
                &self.client,
                &self.agent,
                runtime_data,
                agent_port,
                progress,
            )
            .await?
        } else {
            JobContainer::create_agent(
                &self.client,
                &self.agent,
                runtime_data,
                agent_port,
                progress,
            )
            .await?
        };

        Ok((pod_agent_name, agent_port))
    }
}
