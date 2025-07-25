use std::ops::{Deref, Not};

use k8s_openapi::NamespaceResourceScope;
use kube::{
    config::{KubeConfigOptions, Kubeconfig},
    Api, Client, Config, Discovery,
};
use mirrord_agent_env::mesh::MeshVendor;
use mirrord_config::{
    agent::AgentConfig,
    feature::network::NetworkConfig,
    target::{Target, TargetConfig},
    LayerConfig,
};
use mirrord_progress::Progress;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, Level};

use super::container::ContainerConfig;
use crate::{
    api::{
        container::{
            ephemeral::EphemeralTargetedVariant,
            job::{JobTargetedVariant, JobVariant},
            targeted::Targeted,
            targetless::Targetless,
            ContainerApi, ContainerParams,
        },
        runtime::{RuntimeData, RuntimeDataProvider},
    },
    error::{KubeApiError, Result},
};

#[cfg(feature = "portforward")]
pub mod portforwarder;
pub mod rollout;
pub mod seeker;

pub struct KubernetesAPI {
    client: Client,
    agent: AgentConfig,
}

impl KubernetesAPI {
    /// Creates a new instance from the given [`LayerConfig`].
    ///
    /// If [`LayerConfig::target`] specifies a targetless run,
    /// replaces [`AgentConfig::namespace`] with the target namespace.
    pub async fn create(config: &LayerConfig) -> Result<Self> {
        let client = create_kube_config(
            config.accept_invalid_certificates,
            config.kubeconfig.clone(),
            config.kube_context.clone(),
        )
        .await?
        .try_into()?;

        let mut agent = config.agent.clone();
        if config
            .target
            .path
            .as_ref()
            .is_none_or(|path| matches!(path, Target::Targetless))
        {
            agent.namespace = config.target.namespace.clone();
        }

        Ok(KubernetesAPI::new(client, agent))
    }

    pub fn new(client: Client, agent: AgentConfig) -> Self {
        KubernetesAPI { client, agent }
    }

    /// Returns a reference to the [`Client`] used by this instance.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Returns a reference to the [`AgentConfig`] used by this instance.
    pub fn agent_config(&self) -> &AgentConfig {
        &self.agent
    }

    pub async fn detect_openshift<P>(&self, progress: &P) -> Result<()>
    where
        P: Progress + Send + Sync,
    {
        // filter openshift to make it a lot faster
        if Discovery::new(self.client.clone())
            .filter(&["route.openshift.io"])
            .run()
            .await?
            .has_group("route.openshift.io")
        {
            progress.warning("mirrord has detected it's running on OpenShift. Due to the default PSP of OpenShift, mirrord may not be able to create the agent. Please refer to the documentation at https://metalbear.co/mirrord/docs/faq/limitations/#does-mirrord-support-openshift");
        } else {
            debug!("OpenShift was not detected.");
        }
        Ok(())
    }

    /// Connect to the agent using plain TCP connection.
    #[cfg(feature = "incluster")]
    pub async fn create_connection(
        &self,
        AgentKubernetesConnectInfo {
            pod_name,
            pod_namespace,
            agent_port,
            ..
        }: &AgentKubernetesConnectInfo,
    ) -> Result<tokio::net::TcpStream> {
        use std::{net::IpAddr, time::Duration};

        use k8s_openapi::api::core::v1::Pod;
        use tokio::net::TcpStream;

        let pod_api: Api<Pod> = Api::namespaced(self.client.clone(), pod_namespace);

        let pod = pod_api.get(pod_name).await?;

        let pod_ip = pod
            .status
            .as_ref()
            .and_then(|status| status.pod_ip.as_ref());
        let conn = if let Some(pod_ip) = pod_ip {
            // When pod_ip is available we directly create it as SocketAddr to prevent tokio from
            // performing a DNS lookup.
            let ip = pod_ip
                .parse::<IpAddr>()
                .map_err(|e| KubeApiError::invalid_value(&pod, "status.podIp", e))?;
            tracing::trace!("connecting to pod {pod_ip}:{agent_port}");

            tokio::time::timeout(
                Duration::from_secs(self.agent.startup_timeout),
                TcpStream::connect((ip, *agent_port)),
            )
            .await
            .map_err(|_| KubeApiError::AgentReadyTimeout)??
        } else {
            let hostname = format!("{pod_name}.{pod_namespace}");
            tracing::trace!("connecting to pod {hostname}:{agent_port}");

            tokio::time::timeout(
                Duration::from_secs(self.agent.startup_timeout),
                TcpStream::connect((hostname.as_str(), *agent_port)),
            )
            .await
            .map_err(|_| KubeApiError::AgentReadyTimeout)??
        };

        Ok(conn)
    }

    /// Connects to the agent using kube's [`Api::portforward`].
    #[cfg(feature = "portforward")]
    pub async fn create_connection_portforward(
        &self,
        connect_info: AgentKubernetesConnectInfo,
    ) -> Result<Box<dyn UnpinStream>> {
        let (stream, portforward) =
            portforwarder::retry_portforward(&self.client, connect_info).await?;

        tokio::spawn(portforward.into_retry_future());

        Ok(stream)
    }

    /// Prepares params to create an agent.
    ///
    /// Unless targetless, fetches [`RuntimeData`] for the given target and fills
    /// [`ContainerConfig::pod_ips`].
    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err)]
    pub async fn create_agent_params(
        &self,
        target: &TargetConfig,
        mut config: ContainerConfig,
    ) -> Result<(ContainerParams, Option<RuntimeData>), KubeApiError> {
        let runtime_data = match target.path.as_ref().unwrap_or(&Target::Targetless) {
            Target::Targetless => None,
            path => path
                .runtime_data(&self.client, target.namespace.as_deref())
                .await?
                .into(),
        };

        let pod_ips = runtime_data
            .as_ref()
            .map(|runtime_data| runtime_data.pod_ips.clone())
            .filter(|pod_ips| !pod_ips.is_empty());

        config.pod_ips = pod_ips;

        Ok((config.into(), runtime_data))
    }

    /// Creates an agent.
    ///
    /// Unless targetless, fetches [`RuntimeData`] for the given target and fills
    /// [`ContainerConfig::pod_ips`].
    #[tracing::instrument(level = "trace", skip(self, progress))]
    pub async fn create_agent<P>(
        &self,
        progress: &mut P,
        target_config: &TargetConfig,
        network_config: Option<&mut NetworkConfig>,
        container_config: ContainerConfig,
    ) -> Result<AgentKubernetesConnectInfo, KubeApiError>
    where
        P: Progress + Send + Sync,
    {
        let (params, runtime_data) = self
            .create_agent_params(target_config, container_config)
            .await?;

        if let Some(RuntimeData {
            guessed_container,
            container_name,
            containers_probe_ports,
            ..
        }) = runtime_data.as_ref()
        {
            if *guessed_container {
                progress.warning(format!("Target has multiple containers, mirrord picked \"{container_name}\". To target a different one, include it in the target path.").as_str());
            }

            if let Some(network_config) = network_config {
                if let Some(modified_ports) = network_config
                    .incoming
                    .add_probe_ports_to_http_filter_ports(containers_probe_ports)
                {
                    progress.info(&format!("`network.incoming.http_filter.ports` has been set to use ports {modified_ports}."));
                }

                let stolen_probes = containers_probe_ports
                    .iter()
                    .copied()
                    .filter(|port| network_config.incoming.steals_port_without_filter(*port))
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");

                if stolen_probes.is_empty().not() {
                    progress.warning(&format!(
                    "Your mirrord config may steal HTTP/gRPC health checks configured on ports [{}], \
                    causing Kubernetes to terminate containers on the targeted pod. \
                    Use an HTTP filter to prevent this.",
                    stolen_probes,
                ));
                }
            }
        }

        if let Some(mesh) = runtime_data.as_ref().and_then(|data| data.mesh.as_ref()) {
            progress.info(&format!("service mesh detected: {mesh}"));

            if matches!(mesh, MeshVendor::IstioAmbient) && self.agent.privileged.not() {
                progress.warning(
                    "mirrord detected an ambient Istio service mesh but \
                     the agent is not configured to run in a privileged SecurityContext.\
                     Please set `agent.privileged = true`, otherwise the agent will not be able to start.",
                );
            }
        }

        info!(?params, "Spawning new agent");

        let agent_connect_info = match (runtime_data, self.agent.ephemeral) {
            (None, false) => {
                let variant = JobVariant::new(&self.agent, &params);

                Targetless::new(&self.client, &variant)
                    .create_agent(progress)
                    .await?
            }
            (Some(runtime_data), false) => {
                let variant = JobTargetedVariant::new(&self.agent, &params, &runtime_data);

                Targeted::new(&self.client, &runtime_data, &variant)
                    .create_agent(progress)
                    .await?
            }
            (Some(runtime_data), true) => {
                let variant = EphemeralTargetedVariant::new(&self.agent, &params, &runtime_data);

                Targeted::new(&self.client, &runtime_data, &variant)
                    .create_agent(progress)
                    .await?
            }
            (None, true) => return Err(KubeApiError::MissingRuntimeData),
        };

        info!(?agent_connect_info, "Created agent pod");

        Ok(agent_connect_info)
    }
}

/// Trait for IO streams returned from [`KubernetesAPI::create_connection_portforward`].
/// It's here only to group the exisiting traits we actually need and return a `Box<dyn ...>`
#[cfg(feature = "portforward")]
pub trait UnpinStream:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static
{
}

/// Any type that implements bidirectional IO and can be sent to a different [`tokio::task`] is good
/// enough.
#[cfg(feature = "portforward")]
impl<T> UnpinStream for T where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static
{
}

/// Provides information necessary to make a connection to a running mirrord agent.
#[derive(Debug, Clone, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct AgentKubernetesConnectInfo {
    /// Name of the pod that hosts the agent container.
    pub pod_name: String,
    /// Namespace where the pod hosting the agent container lives.
    pub pod_namespace: String,
    /// Port on which the agent accepts connections.
    pub agent_port: u16,
}

pub async fn create_kube_config<P>(
    accept_invalid_certificates: Option<bool>,
    kubeconfig: Option<P>,
    kube_context: Option<String>,
) -> Result<Config>
where
    P: AsRef<str>,
{
    let kube_config_opts = KubeConfigOptions {
        context: kube_context,
        ..Default::default()
    };

    let mut config = if let Some(kubeconfig) = kubeconfig {
        let kubeconfig = shellexpand::full(&kubeconfig)
            .map_err(|e| KubeApiError::ConfigPathExpansionError(e.to_string()))?;
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

    if let Some(accept_invalid_certificates) = accept_invalid_certificates {
        config.accept_invalid_certs = accept_invalid_certificates;
    }

    Ok(config)
}

#[tracing::instrument(level = "trace", skip(client))]
pub fn get_k8s_resource_api<K>(client: &Client, namespace: Option<&str>) -> Api<K>
where
    K: kube::Resource<Scope = NamespaceResourceScope>,
    <K as kube::Resource>::DynamicType: Default,
{
    if let Some(namespace) = namespace {
        Api::namespaced(client.clone(), namespace)
    } else {
        Api::default_namespaced(client.clone())
    }
}
