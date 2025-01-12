use std::ops::Deref;

use k8s_openapi::{api::core::v1::Namespace, NamespaceResourceScope};
use kube::{
    api::ListParams,
    config::{KubeConfigOptions, Kubeconfig},
    Api, Client, Config, Discovery,
};
use mirrord_config::{
    agent::AgentConfig,
    target::{Target, TargetConfig},
    LayerConfig,
};
use mirrord_progress::Progress;
use mirrord_protocol::MeshVendor;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

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
    pub async fn create(config: &LayerConfig) -> Result<Self> {
        let client = create_kube_config(
            config.accept_invalid_certificates,
            config.kubeconfig.clone(),
            config.kube_context.clone(),
        )
        .await?
        .try_into()?;

        Ok(KubernetesAPI::new(client, config.agent.clone()))
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
            progress.warning("mirrord has detected it's running on OpenShift. Due to the default PSP of OpenShift, mirrord may not be able to create the agent. Please refer to the documentation at https://mirrord.dev/docs/faq/limitations/#does-mirrord-support-openshift");
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
            agent_port,
            namespace,
            ..
        }: AgentKubernetesConnectInfo,
    ) -> Result<tokio::net::TcpStream> {
        use std::{net::IpAddr, time::Duration};

        use k8s_openapi::api::core::v1::Pod;
        use tokio::net::TcpStream;

        let pod_api: Api<Pod> = get_k8s_resource_api(&self.client, namespace.as_deref());

        let pod = pod_api.get(&pod_name).await?;

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
                TcpStream::connect((ip, agent_port)),
            )
            .await
            .map_err(|_| KubeApiError::AgentReadyTimeout)??
        } else {
            let hostname = match namespace {
                Some(namespace) => format!("{pod_name}.{namespace}"),
                None => pod_name,
            };
            tracing::trace!("connecting to pod {hostname}:{agent_port}");

            tokio::time::timeout(
                Duration::from_secs(self.agent.startup_timeout),
                TcpStream::connect((hostname.as_str(), agent_port)),
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

    /// # Params
    ///
    /// * `config` - if passed, will be checked against cluster setup
    /// * `tls_cert` - value for
    ///   [`AGENT_OPERATOR_CERT_ENV`](mirrord_protocol::AGENT_OPERATOR_CERT_ENV), for creating an
    ///   agent from the operator. In usage from this repo this is always `None`.
    #[tracing::instrument(level = "trace", skip(self), ret, err)]
    pub async fn create_agent_params(
        &self,
        target: &TargetConfig,
        tls_cert: Option<String>,
        support_ipv6: bool,
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
            .filter(|runtime_data| !runtime_data.pod_ips.is_empty())
            .map(|runtime_data| {
                runtime_data
                    .pod_ips
                    .iter()
                    .map(|ip| ip.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            });

        let params = ContainerParams::new(tls_cert, pod_ips, support_ipv6);

        Ok((params, runtime_data))
    }

    /// # Params
    ///
    /// * `config` - if passed, will be checked against cluster setup
    /// * `tls_cert` - value for
    ///   [`AGENT_OPERATOR_CERT_ENV`](mirrord_protocol::AGENT_OPERATOR_CERT_ENV), for creating an
    ///   agent from the operator. In usage from this repo this is always `None`.
    #[tracing::instrument(level = "trace", skip(self, progress))]
    pub async fn create_agent<P>(
        &self,
        progress: &mut P,
        target: &TargetConfig,
        config: Option<&LayerConfig>,
        tls_cert: Option<String>,
    ) -> Result<AgentKubernetesConnectInfo, KubeApiError>
    where
        P: Progress + Send + Sync,
    {
        let support_ipv6 = config
            .map(|layer_conf| layer_conf.feature.network.ipv6)
            .unwrap_or_default();
        let (params, runtime_data) = self
            .create_agent_params(target, tls_cert, support_ipv6)
            .await?;
        if let Some(RuntimeData {
            guessed_container: true,
            container_name,
            ..
        }) = runtime_data.as_ref()
        {
            progress.warning(format!("Target has multiple containers, mirrord picked \"{container_name}\". To target a different one, include it in the target path.").as_str());
        }

        if let Some(mesh) = runtime_data.as_ref().and_then(|data| data.mesh.as_ref()) {
            progress.info(&format!("service mesh detected: {mesh}"));

            let privileged = config
                .map(|config| config.agent.privileged)
                .unwrap_or_default();

            if matches!(mesh, MeshVendor::IstioAmbient) && !privileged {
                progress.warning(
                    "mirrord detected an ambient Istio service mesh but\
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

#[derive(Debug, Clone, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct AgentKubernetesConnectInfo {
    pub pod_name: String,
    pub agent_port: u16,
    pub namespace: Option<String>,
    pub agent_version: Option<String>,
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

/// Get a vector of namespaces from an optional namespace.
///
/// If the given namespace is Some, then
/// fetch its Namespace object, and return a vector only with that.
/// If the namespace is None - return all namespaces.
pub async fn get_namespaces(
    client: &Client,
    namespace: Option<&str>,
    lp: &ListParams,
) -> Result<Vec<Namespace>> {
    let api: Api<Namespace> = Api::all(client.clone());
    Ok(if let Some(namespace) = namespace {
        vec![api.get(namespace).await?]
    } else {
        api.list(lp).await?.items
    })
}

/// Check if the client can see a given namespace.
pub async fn namespace_exists_for_client(namespace: &str, client: &Client) -> bool {
    let api: Api<Namespace> = Api::all(client.clone());
    api.get(namespace).await.is_ok()
}
