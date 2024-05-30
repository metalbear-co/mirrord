use std::{collections::HashMap, fmt, path::Path};

use k8s_openapi::api::core::v1::{ResourceRequirements, Toleration};
use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{
    self, from_env::FromEnv, source::MirrordConfigSource, ConfigContext, ConfigError,
    FromMirrordConfig, MirrordConfig,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LinuxCapability {
    SysAdmin,
    SysPtrace,
    NetRaw,
    NetAdmin,
}

impl LinuxCapability {
    /// All capabilities that can be used by the agent.
    pub fn all() -> &'static [Self] {
        &[
            Self::SysAdmin,
            Self::SysPtrace,
            Self::NetRaw,
            Self::NetAdmin,
        ]
    }
}

impl fmt::Display for LinuxCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::SysAdmin => "SYS_ADMIN",
            Self::SysPtrace => "SYS_PTRACE",
            Self::NetRaw => "NET_RAW",
            Self::NetAdmin => "NET_ADMIN",
        };

        f.write_str(as_str)
    }
}

/// Configuration for the mirrord-agent pod that is spawned in the Kubernetes cluster.
///
/// We provide sane defaults for this option, so you don't have to set up anything here.
///
/// ```json
/// {
///   "agent": {
///     "log_level": "info",
///     "json_log": false,
///     "namespace": "default",
///     "image": "ghcr.io/metalbear-co/mirrord:latest",
///     "image_pull_policy": "IfNotPresent",
///     "image_pull_secrets": [ { "secret-key": "secret" } ],
///     "ttl": 30,
///     "ephemeral": false,
///     "communication_timeout": 30,
///     "startup_timeout": 360,
///     "network_interface": "eth0",
///     "flush_connections": false,
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug)]
#[config(map_to = "AgentFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq"))]
pub struct AgentConfig {
    /// ### agent.log_level {#agent-log_level}
    ///
    /// Log level for the agent.
    ///
    ///
    /// Supports `"trace"`, `"debug"`, `"info"`, `"warn"`, `"error"`, or any string that would work
    /// with `RUST_LOG`.
    ///
    /// ```json
    /// {
    ///   "agent": {
    ///     "log_level": "mirrord=debug,warn"
    ///   }
    /// }
    /// ```
    #[config(env = "MIRRORD_AGENT_RUST_LOG", default = "info")]
    pub log_level: String,

    /// ### agent.json_log {#agent-json_log}
    ///
    /// Controls whether the agent produces logs in a human-friendly format, or json.
    ///
    /// ```json
    /// {
    ///   "agent": {
    ///     "json_log": true
    ///   }
    /// }
    /// ```
    #[config(env = "MIRRORD_AGENT_JSON_LOG", default = false)]
    pub json_log: bool,

    /// ### agent.namespace {#agent-namespace}
    ///
    /// Namespace where the agent shall live.
    /// Note: Doesn't work with ephemeral containers.
    /// Defaults to the current kubernetes namespace.
    #[config(env = "MIRRORD_AGENT_NAMESPACE")]
    pub namespace: Option<String>,

    /// ### agent.image {#agent-image}
    ///
    /// Name of the agent's docker image.
    ///
    /// Useful when a custom build of mirrord-agent is required, or when using an internal
    /// registry.
    ///
    /// Defaults to the latest stable image `"ghcr.io/metalbear-co/mirrord:latest"`.
    ///
    /// ```json
    /// {
    ///   "image": "internal.repo/images/mirrord:latest"
    /// }
    /// ```
    ///
    /// Complete setup:
    ///
    /// ```json
    /// {
    ///   "image": {
    ///     "registry": "internal.repo/images/mirrord",
    ///     "tag": "latest",
    ///   }
    /// }
    /// ```
    #[config(nested)]
    pub image: AgentImageConfig,

    /// ### agent.image_pull_policy {#agent-image_pull_policy}
    ///
    /// Controls when a new agent image is downloaded.
    ///
    /// Supports `"IfNotPresent"`, `"Always"`, `"Never"`, or any valid kubernetes
    /// [image pull policy](https://kubernetes.io/docs/concepts/containers/images/#image-pull-policy)
    ///
    /// Defaults to `"IfNotPresent"`
    #[config(env = "MIRRORD_AGENT_IMAGE_PULL_POLICY", default = "IfNotPresent")]
    pub image_pull_policy: String,

    /// ### agent.image_pull_secrets {#agent-image_pull_secrets}
    ///
    /// List of secrets the agent pod has access to.
    ///
    /// Takes an array of entries with the format `{ name: <secret-name> }`.
    ///
    /// Read more [here](https://kubernetes.io/docs/concepts/containers/images/#referring-to-an-imagepullsecrets-on-a-pod).
    ///
    /// ```json
    /// {
    ///   "agent": {
    ///     "image_pull_secrets": [
    ///       { "name": "secret-key-1" },
    ///       { "name": "secret-key-2" }
    ///     ]
    ///   }
    /// }
    /// ```
    pub image_pull_secrets: Option<Vec<AgentPullSecret>>,

    /// ### agent.ttl {#agent-ttl}
    ///
    /// Controls how long the agent pod persists for after the agent exits (in seconds).
    ///
    /// Can be useful for collecting logs.
    ///
    /// Defaults to `1`.
    #[config(env = "MIRRORD_AGENT_TTL", default = 1)]
    pub ttl: u16,

    /// ### agent.ephemeral {#agent-ephemeral}
    ///
    /// Runs the agent as an
    /// [ephemeral container](https://kubernetes.io/docs/concepts/workloads/pods/ephemeral-containers/)
    ///
    /// Defaults to `false`.
    #[config(env = "MIRRORD_EPHEMERAL_CONTAINER", default = false)]
    pub ephemeral: bool,

    /// ### agent.communication_timeout {#agent-communication_timeout}
    ///
    /// Controls how long the agent lives when there are no connections.
    ///
    /// Each connection has its own heartbeat mechanism, so even if the local application has no
    /// messages, the agent stays alive until there are no more heartbeat messages.
    #[config(env = "MIRRORD_AGENT_COMMUNICATION_TIMEOUT")]
    pub communication_timeout: Option<u16>,

    /// ### agent.startup_timeout {#agent-startup_timeout}
    ///
    /// Controls how long to wait for the agent to finish initialization.
    ///
    /// If initialization takes longer than this value, mirrord exits.
    ///
    /// Defaults to `60`.
    #[config(env = "MIRRORD_AGENT_STARTUP_TIMEOUT", default = 60)]
    pub startup_timeout: u64,

    /// ### agent.network_interface {#agent-network_interface}
    ///
    /// Which network interface to use for mirroring.
    ///
    /// The default behavior is try to access the internet and use that interface. If that fails
    /// it uses `eth0`.
    #[config(env = "MIRRORD_AGENT_NETWORK_INTERFACE")]
    pub network_interface: Option<String>,

    /// ### agent.flush_connections {#agent-flush_connections}
    ///
    /// Flushes existing connections when starting to steal, might fix issues where connections
    /// aren't stolen (due to being already established)
    ///
    /// Defaults to `true`.
    // Temporary fix for issue [#1029](https://github.com/metalbear-co/mirrord/issues/1029).
    #[config(
        env = "MIRRORD_AGENT_STEALER_FLUSH_CONNECTIONS",
        default = true,
        unstable
    )]
    pub flush_connections: bool,

    /// ### agent.disabled_capabilities {#agent-disabled_capabilities}
    ///
    /// Disables specified Linux capabilities for the agent container.
    /// If nothing is disabled here, agent uses `NET_ADMIN`, `NET_RAW`, `SYS_PTRACE` and
    /// `SYS_ADMIN`.
    pub disabled_capabilities: Option<Vec<LinuxCapability>>,

    /// ### agent.tolerations {#agent-tolerations}
    ///
    /// Set pod tolerations. (not with ephemeral agents)
    /// Default is
    /// ```json
    /// [
    ///   {
    ///     "operator": "Exists"
    ///   }
    /// ]
    /// ```
    ///
    /// Set to an empty array to have no tolerations at all
    pub tolerations: Option<Vec<Toleration>>,

    /// ### agent.resources {#agent-resources}
    ///
    /// Set pod resource reqirements. (not with ephemeral agents)
    /// Default is
    /// ```json
    /// {
    ///   "requests":
    ///   {
    ///     "cpu": "1m",
    ///     "memory": "1Mi"
    ///   },
    ///   "limits":
    ///   {
    ///     "cpu": "100m",
    ///       "memory": "100Mi"
    ///   }
    /// }
    /// ```
    pub resources: Option<ResourceRequirements>,

    /// ### agent.check_out_of_pods {#agent-check_out_of_pods}
    ///
    /// Determine if to check whether there is room for agent job in target node. (Not applicable
    /// when using ephemeral containers feature)
    ///
    /// Can be disabled if the check takes too long and you are sure there is enough resources on
    /// each node
    #[config(default = true)]
    pub check_out_of_pods: bool,

    /// ### agent.privileged {#agent-privileged}
    ///
    /// Run the mirror agent as privileged container.
    /// Defaults to `false`.
    ///
    /// Might be needed in strict environments such as Bottlerocket.
    #[config(default = false)]
    pub privileged: bool,

    /// ### agent.nftables {#agent-nftables}
    ///
    /// Use iptables-nft instead of iptables-legacy.
    /// Defaults to `false`.
    ///
    /// Needed if your mesh uses nftables instead of iptables-legacy,
    #[config(default = false)]
    pub nftables: bool,

    /// ### agent.dns {#agent-dns}
    #[config(nested)]
    pub dns: AgentDnsConfig,

    /// ### agent.labels {#agent-labels}
    ///
    /// Allows setting up custom labels for the agent Job and Pod.
    ///
    /// ```json
    /// {
    ///   "labels": { "user": "meow", "state": "asleep" }
    /// }
    /// ```
    pub labels: Option<HashMap<String, String>>,

    /// ### agent.annotations {#agent-annotations}
    ///
    /// Allows setting up custom annotations for the agent Job and Pod.
    ///
    /// ```json
    /// {
    ///   "annotations": { "cats.io/inject": "enabled" }
    /// }
    /// ```
    pub annotations: Option<HashMap<String, String>>,

    /// <!--${internal}-->
    /// Create an agent that returns an error after accepting the first client. For testing
    /// purposes. Only supported with job agents (not with ephemeral agents).
    #[cfg(all(debug_assertions, not(test)))] // not(test) so that it's not included in the schema json.
    #[config(env = "MIRRORD_AGENT_TEST_ERROR", default = false, unstable)]
    pub test_error: bool,
}

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AgentImageConfig(pub String);

impl Default for AgentImageConfig {
    fn default() -> Self {
        Self(format!(
            "{DEFAULT_AGENT_IMAGE_REGISTRY}:{}",
            env!("CARGO_PKG_VERSION")
        ))
    }
}

/// <!--${internal}-->
/// Allows us to support the dual configuration for the agent image.
///
/// Whatever values missing are replaced with our defaults.
#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(untagged, rename_all = "lowercase", deny_unknown_fields)]
pub enum AgentImageFileConfig {
    /// The shortened version of: `image: "repo/mirrord:latest"`.
    Simple(Option<String>),
    /// Expanded version: `image: { registry: "repo/mirrord", tag: "latest" }`.
    Advanced {
        registry: Option<String>,
        tag: Option<String>,
    },
}

impl Default for AgentImageFileConfig {
    fn default() -> Self {
        Self::Simple(None)
    }
}

impl FromMirrordConfig for AgentImageConfig {
    type Generator = AgentImageFileConfig;
}

/// <!--${internal}-->
/// The default agent image we use together with [`env!`] `CARGO_PKG_VERSION`.
const DEFAULT_AGENT_IMAGE_REGISTRY: &str = "ghcr.io/metalbear-co/mirrord";

impl AgentImageFileConfig {
    fn get_image_from_env(context: &mut ConfigContext) -> config::Result<Option<String>> {
        FromEnv::new("MIRRORD_AGENT_IMAGE")
            .source_value(context)
            .transpose()
    }
}

/// <!--${internal}-->
/// Specifies a secret reference for the agent pod.
#[derive(Clone, Debug, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
pub struct AgentPullSecret {
    /// Name of the secret.
    pub name: String,
}

impl MirrordConfig for AgentImageFileConfig {
    type Generated = AgentImageConfig;

    /// Generates the [`AgentImageConfig`] from the `agent.image` config, or the
    /// `MIRRORD_AGENT_IMAGE` env var.
    fn generate_config(self, context: &mut ConfigContext) -> config::Result<Self::Generated> {
        let agent_image = match self {
            AgentImageFileConfig::Simple(registry_and_tag) => {
                registry_and_tag.unwrap_or_else(|| {
                    format!(
                        "{DEFAULT_AGENT_IMAGE_REGISTRY}:{}",
                        env!("CARGO_PKG_VERSION")
                    )
                })
            }
            AgentImageFileConfig::Advanced { registry, tag } => {
                format!(
                    "{}:{}",
                    registry.unwrap_or_else(|| DEFAULT_AGENT_IMAGE_REGISTRY.to_string()),
                    tag.unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
                )
            }
        };

        // Env overrides configuration if both there.
        let agent_image = Self::get_image_from_env(context)?.unwrap_or(agent_image);

        Ok(AgentImageConfig(agent_image))
    }
}

impl CollectAnalytics for &AgentConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("ephemeral", self.ephemeral);
    }
}

impl AgentConfig {
    pub fn image(&self) -> &str {
        &self.image.0
    }
}

impl AgentFileConfig {
    pub fn from_path<P>(path: P) -> Result<Self, ConfigError>
    where
        P: AsRef<Path>,
    {
        let config = std::fs::read_to_string(path.as_ref())?;

        match path.as_ref().extension().and_then(|os_val| os_val.to_str()) {
            Some("json") => Ok(serde_json::from_str::<Self>(&config)?),
            Some("toml") => Ok(toml::from_str::<Self>(&config)?),
            Some("yaml" | "yml") => Ok(serde_yaml::from_str::<Self>(&config)?),
            _ => Err(ConfigError::UnsupportedFormat),
        }
    }
}

#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct AgentDnsConfig {
    /// ### agent.dns.timeout {#agent-dns-timeout}
    ///
    /// When agent resolves DNS, how long to wait for a response before timeout
    /// By default this is set to 1 (in the agent).
    /// If the value is too high, it might cause internal proxy to timeout and exit.
    pub timeout: Option<u32>,

    /// ### agent.dns.attempts {#agent-dns-attempts}
    ///
    /// When agent resolves DNS, how many attempts before failing.
    /// If the value is too high, it might cause internal proxy to timeout and exit.
    pub attempts: Option<u32>,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        config::{ConfigContext, MirrordConfig},
        util::testing::with_env_vars,
    };

    #[rstest]
    fn default(
        #[values((None, "info"), (Some("trace"), "trace"))] log_level: (Option<&str>, &str),
        #[values((None, None), (Some("app"), Some("app")))] namespace: (Option<&str>, Option<&str>),
        #[values((None, None), (Some(AgentImageConfig("test".to_string())), Some(AgentImageConfig("test".to_string()))))]
        image: (Option<AgentImageConfig>, Option<AgentImageConfig>),
        #[values((None, "IfNotPresent"), (Some("Always"), "Always"))] image_pull_policy: (
            Option<&str>,
            &str,
        ),
        #[values((None, 1), (Some("30"), 30))] ttl: (Option<&str>, u16),
        #[values((None, false), (Some("true"), true))] ephemeral: (Option<&str>, bool),
        #[values((None, None), (Some("30"), Some(30)))] communication_timeout: (
            Option<&str>,
            Option<u16>,
        ),
        #[values((None, 60), (Some("30"), 30))] startup_timeout: (Option<&str>, u64),
    ) {
        let (left_image, right_image) = image;
        let right_image = right_image.unwrap_or_default();
        let agent_image = left_image.map(|i| i.0);
        let image_str = agent_image.as_deref();

        with_env_vars(
            vec![
                ("MIRRORD_AGENT_RUST_LOG", log_level.0),
                ("MIRRORD_AGENT_NAMESPACE", namespace.0),
                ("MIRRORD_AGENT_IMAGE", image_str),
                ("MIRRORD_AGENT_IMAGE_PULL_POLICY", image_pull_policy.0),
                ("MIRRORD_AGENT_TTL", ttl.0),
                ("MIRRORD_EPHEMERAL_CONTAINER", ephemeral.0),
                (
                    "MIRRORD_AGENT_COMMUNICATION_TIMEOUT",
                    communication_timeout.0,
                ),
                ("MIRRORD_AGENT_STARTUP_TIMEOUT", startup_timeout.0),
            ],
            || {
                let mut cfg_context = ConfigContext::default();
                let agent = AgentFileConfig::default()
                    .generate_config(&mut cfg_context)
                    .unwrap();

                assert_eq!(agent.log_level, log_level.1);
                assert_eq!(agent.namespace.as_deref(), namespace.1);
                assert_eq!(agent.image, right_image);
                assert_eq!(agent.image_pull_policy, image_pull_policy.1);
                assert_eq!(agent.ttl, ttl.1);
                assert_eq!(agent.ephemeral, ephemeral.1);
                assert_eq!(agent.communication_timeout, communication_timeout.1);
                assert_eq!(agent.startup_timeout, startup_timeout.1);
            },
        );
    }
}
