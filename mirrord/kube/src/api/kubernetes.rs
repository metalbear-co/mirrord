use std::{
    ffi::OsStr,
    fmt,
    ops::{Deref, Not},
    path::PathBuf,
};

use k8s_openapi::NamespaceResourceScope;
use kube::{
    Api, Client, Config, Discovery,
    client::ClientBuilder,
    config::{KubeConfigOptions, Kubeconfig},
};
use mirrord_agent_env::mesh::MeshVendor;
use mirrord_config::{
    LayerConfig,
    agent::AgentConfig,
    feature::network::NetworkConfig,
    target::{Target, TargetConfig},
};
use mirrord_progress::Progress;
use serde::{Deserialize, Serialize};
use tower::{buffer::BufferLayer, retry::RetryLayer};
use tracing::{Level, debug, info};

use super::container::ContainerConfig;
use crate::{
    api::{
        container::{
            ContainerApi, ContainerParams,
            ephemeral::EphemeralTargetedVariant,
            job::{JobTargetedVariant, JobVariant},
            targeted::Targeted,
            targetless::Targetless,
        },
        runtime::{RuntimeData, RuntimeDataProvider},
    },
    error::{KubeApiError, Result},
    retry::retry_policy_from_config,
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
    pub async fn create<P: Progress>(config: &LayerConfig, progress: &P) -> Result<Self> {
        let client_config = create_kube_config(
            config.accept_invalid_certificates,
            config.kubeconfig.clone(),
            config.kube_context.clone(),
        )
        .await?;

        let client = progress
            .suspend(|| ClientBuilder::try_from(client_config.clone()))?
            .with_layer(&BufferLayer::new(1024))
            .with_layer(&RetryLayer::new(retry_policy_from_config(
                &config.startup_retry,
            )?))
            .build();

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
        P: Progress,
    {
        // filter openshift to make it a lot faster
        if Discovery::new(self.client.clone())
            .filter(&["route.openshift.io"])
            .run()
            .await?
            .has_group("route.openshift.io")
        {
            progress.warning("mirrord has detected it's running on OpenShift. Due to the default PSP of OpenShift, mirrord may not be able to create the agent. Please refer to the documentation at https://metalbear.com/mirrord/docs/faq/limitations/#does-mirrord-support-openshift");
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

        // mirrord protocol messages are small and latency sensitive, buffering them with
        // Nagle's algorithm only slows the session down. Failing to set it costs latency,
        // not correctness, so it must not fail the connection.
        if let Err(error) = conn.set_nodelay(true) {
            tracing::warn!(%error, "Failed to set TCP_NODELAY on the agent connection");
        }

        Ok(conn)
    }

    /// Connects to the agent using kube's [`kube::Api::portforward`].
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
        let mut runtime_data = match target.path.as_ref().unwrap_or(&Target::Targetless) {
            Target::Targetless => None,
            path => path
                .runtime_data(&self.client, target.namespace.as_deref())
                .await?
                .into(),
        };

        if let Some(runtime_data) = runtime_data.as_mut() {
            runtime_data.try_resolve_node_hostname(&self.client).await;
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
        P: Progress,
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

/// Fetches the Kubernetes apiserver version as `(major, minor)`.
pub async fn apiserver_version(client: &Client) -> Result<(u16, u16), KubeApiError> {
    let version = client.apiserver_version().await?;
    let major = parse_version_component("major", &version.major)?;
    let minor = parse_version_component("minor", &version.minor)?;
    Ok((major, minor))
}

fn parse_version_component(field: &'static str, data: &str) -> Result<u16, KubeApiError> {
    // Sometimes version number are not purely numeric ("27+")
    let end = data
        .find(|c: char| c.is_ascii_digit().not())
        .unwrap_or(data.len());

    data[..end]
        .parse()
        .map_err(|_| KubeApiError::InvalidVersionNumber {
            field,
            data: data.to_owned(),
        })
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

/// Splits a kubeconfig setting into individual paths, the same way `KUBECONFIG` is parsed by
/// `kube-client`, supporting platform-separated lists of paths. Borrowed affectionately & with love
/// from <https://docs.rs/kube/latest/kube/config/struct.Kubeconfig.html#method.from_env>
fn split_kubeconfig_paths<P>(kubeconfig: &P) -> Vec<String>
where
    P: AsRef<OsStr> + ?Sized,
{
    std::env::split_paths(kubeconfig)
        .filter_map(|path| {
            let path_str = path.as_os_str().to_string_lossy().into_owned();
            path_str.is_empty().not().then_some(path_str)
        })
        .collect()
}

/// Reads every given path and merges them into a single [`Kubeconfig`], applying shell expansion
/// so that paths like `~/.kube/config` resolve.
fn merge_kubeconfigs(paths: &[String]) -> Result<Kubeconfig> {
    paths
        .iter()
        .try_fold(Kubeconfig::default(), |merged_kubeconfig, path_str| {
            let expanded = shellexpand::full(path_str)
                .map_err(|e| KubeApiError::ConfigPathExpansionError(e.to_string()))?;

            Kubeconfig::read_from(expanded.deref())
                .and_then(|config| merged_kubeconfig.merge(config))
                .map_err(KubeApiError::from)
        })
}

/// Path `kube-client` falls back to when neither the mirrord config nor `KUBECONFIG` names one.
fn default_kubeconfig_path() -> Option<PathBuf> {
    std::env::home_dir().map(|home| home.join(".kube").join("config"))
}

/// The kubeconfig and context mirrord resolved when building its Kubernetes client.
///
/// When mirrord cannot find something in the cluster, by far the most common cause is that the
/// kubeconfig or the selected context points somewhere the user did not intend, rather than the
/// resource genuinely being absent. Errors about missing cluster-side resources are far more
/// actionable when they name the source mirrord actually read, so the user can compare it against
/// the cluster they believe they are targeting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KubeContextInfo {
    /// Kubeconfig path(s) mirrord read, if any could be determined.
    pub kubeconfig: Option<String>,
    /// Context mirrord selected, either explicitly configured or the kubeconfig's
    /// `current-context`.
    pub context: Option<String>,
}

impl KubeContextInfo {
    /// Resolves the kubeconfig and context using the same precedence as [`create_kube_config`]:
    /// the mirrord config wins over `KUBECONFIG`, which wins over the default path, and an
    /// explicitly configured context wins over the kubeconfig's `current-context`.
    ///
    /// Every lookup is best-effort. This exists to improve an error message that is already being
    /// returned, so a failure to read the kubeconfig here leaves the field empty instead of
    /// replacing the original error.
    /// Resolves from the same [`LayerConfig`] fields that [`create_kube_config`] is given.
    pub fn from_config(config: &LayerConfig) -> Self {
        Self::resolve(config.kubeconfig.clone(), config.kube_context.clone())
    }

    pub fn resolve(kubeconfig: Option<String>, kube_context: Option<String>) -> Self {
        let configured = kubeconfig
            .or_else(|| std::env::var("KUBECONFIG").ok())
            .filter(|kubeconfig| kubeconfig.is_empty().not());

        let context = kube_context.or_else(|| {
            let kubeconfig = match configured.as_deref() {
                Some(configured) => merge_kubeconfigs(&split_kubeconfig_paths(configured)).ok(),
                None => Kubeconfig::read().ok(),
            };

            kubeconfig.and_then(|kubeconfig| kubeconfig.current_context)
        });

        Self {
            kubeconfig: configured.or_else(|| {
                default_kubeconfig_path()
                    .filter(|path| path.is_file())
                    .map(|path| path.to_string_lossy().into_owned())
            }),
            context,
        }
    }
}

impl fmt::Display for KubeContextInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.kubeconfig, &self.context) {
            (Some(kubeconfig), Some(context)) => {
                write!(f, "kubeconfig `{kubeconfig}` with context `{context}`")
            }
            (Some(kubeconfig), None) => {
                write!(f, "kubeconfig `{kubeconfig}` with no context selected")
            }
            (None, Some(context)) => write!(f, "context `{context}`"),
            (None, None) => f.write_str("no kubeconfig or context that it could identify"),
        }
    }
}

#[tracing::instrument(level = Level::TRACE, skip(kubeconfig), ret, err)]
pub async fn create_kube_config<P>(
    accept_invalid_certificates: Option<bool>,
    kubeconfig: Option<P>,
    kube_context: Option<String>,
) -> Result<Config>
where
    P: AsRef<OsStr>,
{
    let kube_config_opts = KubeConfigOptions {
        context: kube_context,
        ..Default::default()
    };

    let mut config = if let Some(kubeconfig) = kubeconfig
        && let paths = split_kubeconfig_paths(&kubeconfig)
        && paths.is_empty().not()
    {
        let parsed_kube_config = merge_kubeconfigs(&paths)?;
        Config::from_custom_kubeconfig(parsed_kube_config, &kube_config_opts).await?
    } else if kube_config_opts.context.is_some() {
        // if context is set, it's not in cluster so it has to be a kubeconfig.
        Config::from_kubeconfig(&kube_config_opts).await?
    } else {
        // if context isn't set and user doesn't specify a kubeconfig, we infer which tries
        // local kube or in-cluster configuration.
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

#[cfg(test)]
mod test {
    use std::io::Write;

    use rstest::rstest;

    use super::*;

    const KUBECONFIG_YAML: &str = r#"apiVersion: v1
kind: Config
current-context: staging-eu
clusters:
- name: staging
  cluster:
    server: http://127.0.0.1:8080
contexts:
- name: staging-eu
  context:
    cluster: staging
    user: dev
users:
- name: dev
  user: {}
"#;

    fn write_kubeconfig() -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(KUBECONFIG_YAML.as_bytes()).unwrap();
        file.flush().unwrap();
        file
    }

    /// The whole point of [`KubeContextInfo`] is telling a user which cluster mirrord looked in,
    /// so both names have to survive into the rendered message.
    #[rstest]
    #[case(
        Some("/home/dev/.kube/config"),
        Some("staging-eu"),
        "kubeconfig `/home/dev/.kube/config` with context `staging-eu`"
    )]
    #[case(
        Some("/home/dev/.kube/config"),
        None,
        "kubeconfig `/home/dev/.kube/config` with no context selected"
    )]
    #[case(None, Some("staging-eu"), "context `staging-eu`")]
    #[case(None, None, "no kubeconfig or context that it could identify")]
    fn display_names_both_sources(
        #[case] kubeconfig: Option<&str>,
        #[case] context: Option<&str>,
        #[case] expected: &str,
    ) {
        let info = KubeContextInfo {
            kubeconfig: kubeconfig.map(str::to_owned),
            context: context.map(str::to_owned),
        };

        assert_eq!(info.to_string(), expected);
    }

    /// A user who never sets `kube_context` relies entirely on `current-context`, which is exactly
    /// the case where they are most likely to be pointed at the wrong cluster without knowing.
    #[test]
    fn resolve_reads_current_context_from_the_kubeconfig() {
        let file = write_kubeconfig();
        let path = file.path().to_string_lossy().into_owned();

        let info = KubeContextInfo::resolve(Some(path.clone()), None);

        assert_eq!(info.kubeconfig, Some(path));
        assert_eq!(info.context, Some("staging-eu".to_owned()));
    }

    #[test]
    fn resolve_prefers_the_configured_context() {
        let file = write_kubeconfig();
        let path = file.path().to_string_lossy().into_owned();

        let info = KubeContextInfo::resolve(Some(path), Some("prod-us".to_owned()));

        assert_eq!(info.context, Some("prod-us".to_owned()));
    }

    /// An unreadable kubeconfig must not turn into a hard failure, because this type only ever
    /// decorates an error that is already on its way to the user.
    #[test]
    fn resolve_tolerates_an_unreadable_kubeconfig() {
        let info = KubeContextInfo::resolve(Some("/nonexistent/kubeconfig".to_owned()), None);

        assert_eq!(info.kubeconfig, Some("/nonexistent/kubeconfig".to_owned()));
        assert_eq!(info.context, None);
    }
}
