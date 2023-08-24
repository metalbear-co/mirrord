use std::ops::Deref;
#[cfg(feature = "incluster")]
use std::{net::SocketAddr, time::Duration};

use k8s_openapi::api::core::v1::Pod;
use kube::{
    config::{KubeConfigOptions, Kubeconfig},
    Api, Client, Config, Discovery,
};
use mirrord_config::{agent::AgentConfig, target::TargetConfig, LayerConfig};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use rand::Rng;
use serde::{Deserialize, Serialize};
#[cfg(feature = "incluster")]
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{info, trace};

use crate::{
    api::{
        container::{ContainerApi, EphemeralContainer, JobContainer},
        get_k8s_resource_api,
        runtime::RuntimeDataProvider,
        wrap_raw_connection, AgentManagment,
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
            config.kube_context.clone(),
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

    pub async fn detect_openshift<P>(&self, progress: &mut P) -> Result<()>
    where
        P: Progress + Send + Sync,
    {
        if Discovery::new(self.client.clone())
            .run()
            .await?
            .has_group("route.openshift.io")
        {
            progress.warning("mirrord has detected it's running on OpenShift. Due to the default PSP of OpenShift, mirrord may not be able to create the agent. Please refer to the documentation at https://mirrord.dev/docs/overview/faq/#can-i-use-mirrord-with-openshift");
        } else {
            progress.success(Some("OpenShift was not detected."))
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentKubernetesConnectInfo {
    pub pod_name: String,
    pub agent_port: u16,
    pub namespace: Option<String>,
}

impl AgentManagment for KubernetesAPI {
    type AgentRef = AgentKubernetesConnectInfo;
    type Err = KubeApiError;

    #[cfg(feature = "incluster")]
    async fn create_connection(
        &self,
        AgentKubernetesConnectInfo {
            pod_name,
            agent_port,
            namespace,
        }: Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        let pod_api: Api<Pod> = get_k8s_resource_api(&self.client, namespace.as_deref());

        let pod = pod_api.get(pod_name).await?;

        let conn = if let Some(pod_ip) = pod.status.and_then(|status| status.pod_ip) {
            trace!("connecting to pod_ip {pod_ip}:{}", agent_port);

            // When pod_ip is available we directly create it as SocketAddr to prevent tokio from
            // performing a DNS lookup
            tokio::time::timeout(
                Duration::from_secs(self.agent.startup_timeout),
                TcpStream::connect(SocketAddr::new(pod_ip.parse()?, agent_port)),
            )
            .await
        } else {
            trace!("connecting to pod {pod_name}:{agent_port}");
            let connection_string = if let Some(namespace) = namespace {
                format!("{pod_name}.{namespace}:{agent_port}")
            } else {
                format!("{pod_name}:{agent_port}")
            };
            tokio::time::timeout(
                Duration::from_secs(self.agent.startup_timeout),
                TcpStream::connect(connection_string),
            )
            .await
        }
        .map_err(|_| KubeApiError::AgentReadyTimeout)??;

        Ok(wrap_raw_connection(conn))
    }

    #[cfg(not(feature = "incluster"))]
    async fn create_connection(
        &self,
        connect_info: Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        let pod_api: Api<Pod> =
            get_k8s_resource_api(&self.client, connect_info.namespace.as_deref());
        trace!("port-forward to pod {:?}", &connect_info);
        let mut port_forwarder = pod_api
            .portforward(&connect_info.pod_name, &[connect_info.agent_port])
            .await?;

        Ok(wrap_raw_connection(
            port_forwarder
                .take_stream(connect_info.agent_port)
                .ok_or(KubeApiError::PortForwardFailed)?,
        ))
    }

    async fn create_agent<P>(&self, progress: &P) -> Result<Self::AgentRef, Self::Err>
    where
        P: Progress + Send + Sync,
    {
        let runtime_data = if let Some(ref path) = self.target.path {
            let runtime_data = path
                .runtime_data(&self.client, self.target.namespace.as_deref())
                .await?;

            Some(runtime_data)
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

        let agent_connect_info = if self.agent.ephemeral {
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

        info!("Created agent pod {agent_connect_info:?}");
        Ok(agent_connect_info)
    }
}

pub async fn create_kube_api<P>(
    accept_invalid_certificates: bool,
    kubeconfig: Option<P>,
    kube_context: Option<String>,
) -> Result<Client>
where
    P: AsRef<str>,
{
    let kube_config_opts = KubeConfigOptions {
        context: kube_context,
        ..Default::default()
    };

    let mut config = if let Some(kubeconfig) = kubeconfig {
        let kubeconfig = shellexpand::tilde(&kubeconfig);
        let parsed_kube_config = Kubeconfig::read_from(kubeconfig.deref())?;
        Config::from_custom_kubeconfig(parsed_kube_config, &kube_config_opts).await?
    } else if kube_config_opts.context.is_some() {
        // if context is set, it's not in cluster so it has to be a kubeconfig.
        Config::from_kubeconfig(&kube_config_opts).await?
    } else {
        // if context isn't set and user doesn't specify a kubeconfig, we infer which tries local
        // kube or incluster configuration.
        Config::infer().await?
    };
    config.accept_invalid_certs = accept_invalid_certificates;
    Client::try_from(config).map_err(KubeApiError::from)
}
