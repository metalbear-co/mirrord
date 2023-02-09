#[cfg(feature = "incluster")]
use std::time::Duration;

use async_trait::async_trait;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    config::{KubeConfigOptions, Kubeconfig},
    Api, Client, Config,
};
use mirrord_config::{agent::AgentConfig, target::TargetConfig, LayerConfig};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use rand::Rng;
#[cfg(feature = "incluster")]
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{info, trace, warn};

#[cfg(feature = "env_guard")]
use crate::api::env_guard::EnvVarGuard;
use crate::{
    api::{
        container::{ContainerApi, EphemeralContainer, JobContainer},
        get_k8s_resource_api,
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
    pub async fn create(config: &LayerConfig) -> Result<Self> {
        let client = create_kube_api(Some(config.clone())).await?;

        Ok(KubernetesAPI::new(
            client,
            config.agent.clone(),
            config.target.clone(),
        ))
    }

    pub fn new(client: Client, agent: AgentConfig, target: TargetConfig) -> Self {
        KubernetesAPI {
            client,
            agent,
            target,
        }
    }
}

#[async_trait]
impl AgentManagment for KubernetesAPI {
    type AgentRef = (String, u16);
    type Err = KubeApiError;

    #[cfg(feature = "incluster")]
    async fn create_connection(
        &self,
        (pod_agent_name, agent_port): Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        let pod_api: Api<Pod> = get_k8s_resource_api(&self.client, self.agent.namespace.as_deref());

        let pod_addr = pod_api
            .get(&pod_agent_name)
            .await?
            .status
            .and_then(|status| status.pod_ip.clone())
            .unwrap_or(pod_agent_name);

        let agent_addr = format!("{}:{}", pod_addr, agent_port);

        trace!("connecting to pod {}", &agent_addr);

        let conn = tokio::time::timeout(
            Duration::from_secs(self.agent.startup_timeout),
            TcpStream::connect(&agent_addr),
        )
        .await
        .map_err(|_| KubeApiError::AgentReadyTimeout)??;

        wrap_raw_connection(conn)
    }

    #[cfg(not(feature = "incluster"))]
    async fn create_connection(
        &self,
        (pod_agent_name, agent_port): Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        let pod_api: Api<Pod> = get_k8s_resource_api(&self.client, self.agent.namespace.as_deref());
        trace!("port-forward to pod {}:{}", &pod_agent_name, &agent_port);
        let mut port_forwarder = pod_api.portforward(&pod_agent_name, &[agent_port]).await?;

        wrap_raw_connection(port_forwarder.take_stream(agent_port).unwrap())
    }

    async fn create_agent<P>(&self, progress: &P) -> Result<Self::AgentRef, Self::Err>
    where
        P: Progress + Send + Sync,
    {
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

        let agent_gid: u16 = rand::thread_rng().gen_range(3000..u16::MAX);
        info!("Using group-id `{agent_gid:?}`");

        let pod_agent_name = if self.agent.ephemeral {
            EphemeralContainer::create_agent(
                &self.client,
                &self.agent,
                runtime_data,
                agent_port,
                progress,
                agent_gid,
            )
            .await?
        } else {
            JobContainer::create_agent(
                &self.client,
                &self.agent,
                runtime_data,
                agent_port,
                progress,
                agent_gid,
            )
            .await?
        };

        Ok((pod_agent_name, agent_port))
    }
}

pub async fn create_kube_api(config: Option<LayerConfig>) -> Result<Client> {
    #[cfg(feature = "env_guard")]
    let _guard = EnvVarGuard::new();

    let mut kube_config = Config::infer().await?;

    if let Some(config) = config {
        if let Some(kube_config_file) = config.kubeconfig {
            let parsed_kube_config = Kubeconfig::read_from(kube_config_file)?;
            kube_config =
                Config::from_custom_kubeconfig(parsed_kube_config, &KubeConfigOptions::default())
                    .await?;
        }

        #[cfg_attr(not(feature = "env_guard"), allow(unused_mut))]
        if config.accept_invalid_certificates {
            // Only warn the first time connecting to the agent, not on child processes.
            kube_config.accept_invalid_certs = true;
            if config.connect_agent_name.is_none() {
                warn!("Accepting invalid certificates");
            }
        }
    }
    #[cfg(feature = "env_guard")]
    _guard.prepare_config(&mut kube_config);

    Client::try_from(kube_config).map_err(KubeApiError::from)
}
