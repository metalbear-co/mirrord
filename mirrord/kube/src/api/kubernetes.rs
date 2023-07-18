use std::path::Path;
#[cfg(feature = "incluster")]
use std::{net::SocketAddr, time::Duration};

use k8s_openapi::api::core::v1::Pod;
use kube::{
    config::{KubeConfigOptions, Kubeconfig},
    Api, Client, Config,
};
use mirrord_config::{agent::AgentConfig, target::TargetConfig, LayerConfig};
use mirrord_connection::wrap_raw_connection_keepalive;
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use rand::Rng;
#[cfg(feature = "incluster")]
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{info, trace};

use crate::{
    api::{
        container::{ContainerApi, EphemeralContainer, JobContainer},
        get_k8s_resource_api,
        runtime::RuntimeDataProvider,
        AgentManagment,
    },
    error::{KubeApiError, Result},
};

pub mod rollout;

pub struct KubernetesAPI {
    client: Client,
    agent: AgentConfig,
    target: TargetConfig,
}

impl KubernetesAPI {
    pub async fn create(config: &LayerConfig) -> Result<Self> {
        let client = create_kube_api(
            config.accept_invalid_certificates,
            config.kubeconfig.clone(),
        )
        .await?;

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

impl AgentManagment for KubernetesAPI {
    type AgentRef = (String, u16);
    type Err = KubeApiError;

    #[cfg(feature = "incluster")]
    async fn create_connection(
        &self,
        (pod_agent_name, agent_port): Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        use mirrord_connection::wrap_raw_connection_keepalive;

        let pod_api: Api<Pod> = get_k8s_resource_api(&self.client, self.agent.namespace.as_deref());

        let pod = pod_api.get(&pod_agent_name).await?;

        let conn = if let Some(pod_ip) = pod.status.and_then(|status| status.pod_ip) {
            trace!("connecting to pod_ip {pod_ip}:{agent_port}");

            // When pod_ip is available we directly create it as SocketAddr to prevent tokio from
            // performing a DNS lookup
            tokio::time::timeout(
                Duration::from_secs(self.agent.startup_timeout),
                TcpStream::connect(SocketAddr::new(pod_ip.parse()?, agent_port)),
            )
            .await
        } else {
            trace!("connecting to pod {pod_agent_name}:{agent_port}");

            tokio::time::timeout(
                Duration::from_secs(self.agent.startup_timeout),
                TcpStream::connect(format!("{}:{}", pod_agent_name, agent_port)),
            )
            .await
        }
        .map_err(|_| KubeApiError::AgentReadyTimeout)??;

        Ok(wrap_raw_connection_keepalive(conn))
    }

    #[cfg(not(feature = "incluster"))]
    async fn create_connection(
        &self,
        (pod_agent_name, agent_port): Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        let pod_api: Api<Pod> = get_k8s_resource_api(&self.client, self.agent.namespace.as_deref());
        trace!("port-forward to pod {}:{}", &pod_agent_name, &agent_port);
        let mut port_forwarder = pod_api.portforward(&pod_agent_name, &[agent_port]).await?;

        Ok(wrap_raw_connection_keepalive(
            port_forwarder
                .take_stream(agent_port)
                .ok_or(KubeApiError::PortForwardFailed)?,
        ))
    }

    async fn create_agent<P>(&self, progress: &P) -> Result<Self::AgentRef, Self::Err>
    where
        P: Progress + Send + Sync,
    {
        let runtime_data = if let Some(ref path) = self.target.path {
            Some(
                path.runtime_data(&self.client, self.target.namespace.as_deref())
                    .await?,
            )
        } else {
            // Most users won't see this, since the default log level is error, and also progress
            // reporting overrides logs in this stage of the run.
            info!(
                "No target specified. Spawning a targetless agent - not specifying a node, not \
                impersonating any existing resource. \
                To spawn a targeted agent, please specify a target in the configuration.",
            );
            None
        };

        info!("Spawning new agent.");
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

pub async fn create_kube_api<P>(
    accept_invalid_certificates: bool,
    kubeconfig: Option<P>,
) -> Result<Client>
where
    P: AsRef<Path>,
{
    let mut config = if let Some(kubeconfig) = kubeconfig {
        let parsed_kube_config = Kubeconfig::read_from(kubeconfig)?;
        Config::from_custom_kubeconfig(parsed_kube_config, &KubeConfigOptions::default()).await?
    } else {
        Config::infer().await?
    };
    config.accept_invalid_certs = accept_invalid_certificates;
    Client::try_from(config).map_err(KubeApiError::from)
}
