use std::ops::Deref;
#[cfg(feature = "incluster")]
use std::time::Duration;

use k8s_openapi::{
    api::core::v1::{Namespace, Pod},
    NamespaceResourceScope,
};
use kube::{
    api::ListParams,
    config::{KubeConfigOptions, Kubeconfig},
    Api, Client, Config, Discovery,
};
use mirrord_config::{
    agent::AgentConfig, feature::network::incoming::IncomingMode, target::TargetConfig, LayerConfig,
};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
#[cfg(not(feature = "incluster"))]
use tokio_retry::{
    strategy::{jitter, ExponentialBackoff},
    Retry,
};
use tracing::{debug, info, trace};

#[cfg(feature = "incluster")]
use super::connector::AgentInclusterConnector;
#[cfg(not(feature = "incluster"))]
use super::wrap_raw_connection;
use crate::{
    api::{
        container::{
            ephemeral::EphemeralTargetedVariant,
            job::{JobTargetedVariant, JobVariant},
            targeted::Targeted,
            targetless::Targetless,
            ContainerApi, ContainerParams,
        },
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
    /// Used to create a connection with the agent once it's created.
    #[cfg(feature = "incluster")]
    connector: AgentInclusterConnector,
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
            #[cfg(feature = "incluster")]
            connector: AgentInclusterConnector::Tcp,
        }
    }

    /// Replaces [`Self::connector`].
    #[cfg(feature = "incluster")]
    pub fn with_connector(&mut self, connector: AgentInclusterConnector) -> &mut Self {
        self.connector = connector;
        self
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

    /// Checks if any `ContainerStatus` matches a mesh/sidecar name from our `MESH_LIST`, and the
    /// user is running incoming traffic in `IncomigMode::Mirror` mode, printing a warning if it
    /// does.
    #[tracing::instrument(level = "trace", ret, skip(self, progress))]
    pub async fn detect_mesh_mirror_mode<P>(
        &self,
        progress: &mut P,
        incoming_mode: IncomingMode,
        is_mesh: bool,
    ) -> Result<()>
    where
        P: Progress + Send + Sync,
    {
        if matches!(incoming_mode, IncomingMode::Mirror) && is_mesh {
            progress.warning(
                "mirrord has detected that you might be running on a cluster with a \
                 service mesh and `network.incoming.mode = \"mirror\"`, which is currently \
                 unsupported. You can set `network.incoming.mode` to \"steal\" (check out the\
                 `http_filter` configuration value if you only want to steal some of the traffic).",
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct AgentKubernetesConnectInfo {
    pub pod_name: String,
    pub agent_port: u16,
    pub namespace: Option<String>,
}

impl AgentManagment for KubernetesAPI {
    type AgentRef = AgentKubernetesConnectInfo;
    type Err = KubeApiError;

    /// Connect to the agent using [`Self::connector`].
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

        let pod = pod_api.get(&pod_name).await?;

        let conn = if let Some(pod_ip) = pod.status.and_then(|status| status.pod_ip) {
            // When pod_ip is available we directly create it as SocketAddr to prevent tokio from
            // performing a DNS lookup.
            let ip = pod_ip.parse()?;
            trace!("connecting to pod {pod_ip}:{agent_port}");

            tokio::time::timeout(
                Duration::from_secs(self.agent.startup_timeout),
                self.connector.connect_ip(ip, agent_port),
            )
            .await
            .map_err(|_| KubeApiError::AgentReadyTimeout)??
        } else {
            let hostname = match namespace {
                Some(namespace) => format!("{pod_name}.{namespace}"),
                None => pod_name,
            };
            trace!("connecting to pod {hostname}:{agent_port}");

            tokio::time::timeout(
                Duration::from_secs(self.agent.startup_timeout),
                self.connector
                    .connect_hostname(hostname.as_str(), agent_port),
            )
            .await
            .map_err(|_| KubeApiError::AgentReadyTimeout)??
        };

        Ok(conn)
    }

    /// Connects to the agent using kube's [`Api::portforward`].
    #[cfg(not(feature = "incluster"))]
    async fn create_connection(
        &self,
        connect_info: Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        let pod_api: Api<Pod> =
            get_k8s_resource_api(&self.client, connect_info.namespace.as_deref());
        let retry_strategy = ExponentialBackoff::from_millis(10).map(jitter).take(3);
        let ports = &[connect_info.agent_port];
        let mut port_forwarder = Retry::spawn(retry_strategy, || {
            trace!("port-forward to pod {:?}", &connect_info);
            pod_api.portforward(&connect_info.pod_name, ports)
        })
        .await?;

        Ok(wrap_raw_connection(
            port_forwarder
                .take_stream(connect_info.agent_port)
                .ok_or(KubeApiError::PortForwardFailed)?,
        ))
    }

    #[tracing::instrument(level = "trace", skip(self, progress))]
    async fn create_agent<P>(
        &self,
        progress: &mut P,
        config: Option<&LayerConfig>,
    ) -> Result<Self::AgentRef, Self::Err>
    where
        P: Progress + Send + Sync,
    {
        let runtime_data = if let Some(ref path) = self.target.path
            && !matches!(path, mirrord_config::target::Target::Targetless)
        {
            let runtime_data = path
                .runtime_data(&self.client, self.target.namespace.as_deref())
                .await?;

            Some(runtime_data)
        } else {
            None
        };

        let incoming_mode = config
            .map(|config| config.feature.network.incoming.mode)
            .unwrap_or_default();

        let is_mesh = runtime_data
            .as_ref()
            .map(|runtime| runtime.mesh.is_some())
            .unwrap_or_default();

        if self
            .detect_mesh_mirror_mode(progress, incoming_mode, is_mesh)
            .await
            .is_err()
        {
            progress.warning("couldn't determine mesh / sidecar with mirror mode");
        }

        #[cfg(not(feature = "incluster"))]
        let params = ContainerParams::new();
        #[cfg(feature = "incluster")]
        let params = {
            let mut params = ContainerParams::new();
            params.extra_env.extend(self.connector.agent_extra_env());
            params
        };

        info!("Spawning new agent.");

        info!("Using port `{:?}` for communication", params.port);
        info!("Using group-id `{:?}`", params.gid);
        info!("Using extra env `{:?}", params.extra_env);

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
    config.accept_invalid_certs = accept_invalid_certificates;
    Client::try_from(config).map_err(KubeApiError::from)
}

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

/// Get a vector of namespaces from an optional namespace. If the given namespace is Some, then
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
