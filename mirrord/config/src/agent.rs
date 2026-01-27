use std::{collections::HashMap, fmt, net::SocketAddr, path::Path};

use k8s_openapi::api::core::v1::{ResourceRequirements, Toleration};
use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{
    self, ConfigContext, FromFileError, FromMirrordConfig, MirrordConfig, from_env::FromEnv,
    source::MirrordConfigSource,
};

/// Linux capabilities used by the mirrord-agent container.
#[derive(Clone, Copy, Debug, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LinuxCapability {
    SysAdmin,
    SysPtrace,
    NetAdmin,
}

impl LinuxCapability {
    /// All capabilities that can be used by the agent.
    pub fn all() -> &'static [Self] {
        &[Self::SysAdmin, Self::SysPtrace, Self::NetAdmin]
    }

    pub fn as_spec_str(self) -> &'static str {
        match self {
            Self::SysAdmin => "SYS_ADMIN",
            Self::SysPtrace => "SYS_PTRACE",
            Self::NetAdmin => "NET_ADMIN",
        }
    }
}

impl fmt::Display for LinuxCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_spec_str())
    }
}

/// Configuration for the mirrord-agent pod that is spawned in the Kubernetes cluster.
///
/// **Note:** this configuration is ignored when using the mirrord Operator.
/// Agent configuration is done by the cluster admin.
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
///     "flush_connections": false,
///     "exclude_from_mesh": false
///     "inject_headers": false,
///     "max_body_buffer_size": 65535,
///     "max_body_buffer_timeout": 1000
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize, Deserialize, PartialEq)]
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
    ///
    /// **Note:** ignored in targetless runs or when the agent is run as an ephemeral container.
    ///
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
    ///   "agent": {
    ///     "image": "internal.repo/images/mirrord:latest"
    ///   }
    /// }
    /// ```
    ///
    /// Complete setup:
    ///
    /// ```json
    /// {
    ///   "agent": {
    ///     "image": {
    ///       "registry": "internal.repo/images/mirrord",
    ///       "tag": "latest"
    ///     }
    ///   }
    /// }
    /// ```
    ///
    /// Can also be controlled via `MIRRORD_AGENT_IMAGE`, `MIRRORD_AGENT_IMAGE_REGISTRY`, and
    /// `MIRRORD_AGENT_IMAGE_TAG`. `MIRRORD_AGENT_IMAGE` takes precedence, followed by config
    /// values for registry/tag, then environment variables for registry/tag.
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
    /// [ephemeral container](https://kubernetes.io/docs/concepts/workloads/pods/ephemeral-containers/).
    ///
    /// Not compatible with targetless runs.
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
    /// If nothing is disabled here, agent uses:
    /// 1. `NET_ADMIN`,
    /// 2. `SYS_PTRACE`,
    /// 3. `SYS_ADMIN`.
    ///
    /// Has no effect when using the targetless mode,
    /// as targetless agent containers have no capabilities.
    pub disabled_capabilities: Option<Vec<String>>,

    /// ### agent.tolerations {#agent-tolerations}
    ///
    /// Set pod tolerations. (not with ephemeral agents).
    ///
    /// Defaults to `operator: Exists`.
    ///
    /// ```json
    /// {
    ///   "agent": {
    ///     "tolerations": [
    ///         {
    ///           "key": "meow", "operator": "Exists", "effect": "NoSchedule"
    ///         }
    ///     ]
    ///   }
    /// }
    /// ```
    ///
    /// Set to an empty array to have no tolerations at all
    pub tolerations: Option<Vec<Toleration>>,

    /// ### agent.resources {#agent-resources}
    ///
    /// Set pod resource requirements. (not with ephemeral agents)
    /// Default is
    /// ```json
    /// {
    ///   "agent": {
    ///     "resources": {
    ///       "requests":
    ///       {
    ///         "cpu": "1m",
    ///         "memory": "1Mi"
    ///       },
    ///       "limits":
    ///       {
    ///         "cpu": "100m",
    ///         "memory": "100Mi"
    ///       }
    ///     }
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
    ///
    /// Has no effect when using the targetless mode,
    /// as targetless agent containers are never privileged.
    #[config(default = false)]
    pub privileged: bool,

    /// ### agent.nftables {#agent-nftables}
    ///
    /// Determines which iptables backend will be used for traffic redirection.
    ///
    /// If set to `true`, the agent will use iptables-nft.
    /// If set to `false`, the agent will use iptables-legacy.
    /// If not set, the agent will try to detect the correct backend at runtime.
    pub nftables: Option<bool>,

    /// ### agent.dns {#agent-dns}
    #[config(nested)]
    pub dns: AgentDnsConfig,

    /// ### agent.labels {#agent-labels}
    ///
    /// Allows setting up custom labels for the agent Job and Pod.
    ///
    /// ```json
    /// {
    ///   "agent": {
    ///     "labels": { "user": "meow", "state": "asleep" }
    ///   }
    /// }
    /// ```
    pub labels: Option<HashMap<String, String>>,

    /// ### agent.annotations {#agent-annotations}
    ///
    /// Allows setting up custom annotations for the agent Job and Pod.
    ///
    /// ```json
    /// {
    ///   "agent": {
    ///     "annotations": {
    ///       "cats.io/inject": "enabled"
    ///       "prometheus.io/scrape": "true",
    ///       "prometheus.io/port": "9000"
    ///     }
    ///   }
    /// }
    /// ```
    pub annotations: Option<HashMap<String, String>>,

    /// ### agent.node_selector {#agent-node_selector}
    ///
    /// Allows setting up custom node selector for the agent Pod. Applies only to targetless runs,
    /// as targeted agent always runs on the same node as its target container.
    ///
    /// ```json
    /// {
    ///   "agent": {
    ///     "node_selector": { "kubernetes.io/hostname": "node1" }
    ///   }
    /// }
    /// ```
    pub node_selector: Option<HashMap<String, String>>,

    /// ### agent.service_account {#agent-service_account}
    ///
    /// Allows setting up custom Service Account for the agent Job and Pod.
    ///
    /// ```json
    /// {
    ///   "agent": {
    ///     "service_account": "my-service-account"
    ///   }
    /// }
    /// ```
    pub service_account: Option<String>,

    /// ### agent.metrics {#agent-metrics}
    ///
    /// Enables prometheus metrics for the agent pod.
    ///
    /// You might need to add annotations to the agent pod depending on how prometheus is
    /// configured to scrape for metrics.
    ///
    /// ```json
    /// {
    ///   "agent": {
    ///     "metrics": "0.0.0.0:9000"
    ///   }
    /// }
    /// ```
    pub metrics: Option<SocketAddr>,

    /// ### agent.exclude_from_mesh {#agent-exclude_from_mesh}
    ///
    /// When running the agent as an ephemeral container, use this option to exclude
    /// the agent's port from the service mesh sidecar proxy.
    #[config(env = "MIRRORD_AGENT_EXCLUDE_FROM_MESH", default = false)]
    pub exclude_from_mesh: bool,

    /// ### agent.priority_class {#agent-priority_class}
    ///
    /// Specifies the priority class to assign to the agent pod.
    ///
    /// ```json
    /// {
    ///   "agent": {
    ///     "priority_class": "my-priority-class-name"
    ///   }
    /// }
    /// ```
    ///
    /// In some cases, the agent pod may fail to schedule due to node resource constraints.
    /// Setting a priority class allows you to explicitly assign an existing priority class
    /// from your cluster to the agent pod, increasing its priority relative to other workloads.
    pub priority_class: Option<String>,

    /// ### agent.inject_headers {#agent-inject_headers}
    ///
    /// Sets whether `Mirrord-Agent` headers are injected into HTTP
    /// responses that went through the agent.
    ///
    /// Possible values for the header:
    ///
    /// - `passed-through`: set when the request was not sent to the local app (perhaps because it
    ///   didn't match the filters)
    ///
    /// - `forwarded-to-client`: set when the request was sent to the local app
    #[config(default = false)]
    pub inject_headers: bool,

    /// ### agent.max_body_buffer_size {#agent-max_body_buffer_size}
    ///
    /// Maximum size, in bytes, of HTTP request body buffers. Used for
    /// temporarily storing bodies of incoming HTTP requests to run
    /// body filters. HTTP body filters will not match any requests
    /// with bodies larger than this.
    #[config(default = 65535)]
    pub max_body_buffer_size: u32,

    /// ### agent.max_body_buffer_timeout {#agent-max_body_buffer_timeout}
    ///
    /// Maximum timeout, in milliseconds, for receiving HTTP request
    /// bodies. HTTP body filters will not match any requests whose
    /// bodies do not arrive within this timeout.
    #[config(default = 1000)]
    pub max_body_buffer_timeout: u32,

    /// ### agent.security_context {#agent-security_context}
    ///
    /// Agent pod security context (not with ephemeral agents).
    /// Support seccomp profile and app armor profile.
    pub security_context: Option<SecurityContext>,

    /// ### agent.clean_iptables_on_start {#agent-clean_iptables_on_start}
    ///
    /// Clean leftover iptables rules and start the new agent instead of erroring out when there
    /// are existing mirrord rules in the target's iptables.
    #[config(env = "MIRRORD_AGENT_CLEAN_IPTABLES_ON_START")]
    pub clean_iptables_on_start: Option<bool>,

    /// ### agent.disable_mesh_sidecar_injection {#agent-disable_mesh_sidecar_injection}
    ///
    /// Add relevant labels and annotations to agent pods/jobs to
    /// prevent service mesh sidecar injections. Defaults to true.
    ///
    /// Only affects istio, linkerd, kuma.
    #[config(default = true)]
    pub disable_mesh_sidecar_injection: bool,

    /// <!--${internal}-->
    /// Create an agent that returns an error after accepting the first client. For testing
    /// purposes. Only supported with job agents (not with ephemeral agents).
    #[cfg(all(debug_assertions, not(test)))] // not(test) so that it's not included in the schema json.
    #[serde(skip)]
    #[config(env = "MIRRORD_AGENT_TEST_ERROR", default = false, unstable)]
    pub test_error: bool,
}

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug)]
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
        let env_registry = FromEnv::new("MIRRORD_AGENT_IMAGE_REGISTRY")
            .source_value(context)
            .transpose()
            .ok()
            .flatten()
            .unwrap_or_else(|| DEFAULT_AGENT_IMAGE_REGISTRY.to_string());

        let env_tag = FromEnv::new("MIRRORD_AGENT_IMAGE_TAG")
            .source_value(context)
            .transpose()
            .ok()
            .flatten()
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

        let agent_image = match self {
            AgentImageFileConfig::Simple(registry_and_tag) => {
                registry_and_tag.unwrap_or_else(|| format!("{env_registry}:{env_tag}"))
            }
            AgentImageFileConfig::Advanced { registry, tag } => {
                format!(
                    "{}:{}",
                    registry.unwrap_or(env_registry),
                    tag.unwrap_or(env_tag)
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
    pub fn from_path<P>(path: P) -> Result<Self, FromFileError>
    where
        P: AsRef<Path>,
    {
        let config = std::fs::read_to_string(path.as_ref())?;

        match path.as_ref().extension().and_then(|os_val| os_val.to_str()) {
            Some("json") => Ok(serde_json::from_str::<Self>(&config)?),
            Some("toml") => Ok(toml::from_str::<Self>(&config)?),
            Some("yaml" | "yml") => Ok(serde_yaml::from_str::<Self>(&config)?),
            ext => Err(FromFileError::InvalidExtension(ext.map(String::from))),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct SecurityContext {
    pub app_armor_profile: Option<AppArmorProfile>,
    pub seccomp_profile: Option<SeccompProfile>,
}

impl From<SecurityContext> for k8s_openapi::api::core::v1::PodSecurityContext {
    fn from(ctx: SecurityContext) -> Self {
        Self {
            app_armor_profile: ctx.app_armor_profile.map(Into::into),
            seccomp_profile: ctx.seccomp_profile.map(Into::into),
            ..Default::default()
        }
    }
}

pub type AppArmorProfile = SecurityProfile;
pub type SeccompProfile = SecurityProfile;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct SecurityProfile {
    pub localhost_profile: Option<String>,

    #[serde(rename = "type")]
    pub type_: String,
}

impl From<AppArmorProfile> for k8s_openapi::api::core::v1::AppArmorProfile {
    fn from(profile: SecurityProfile) -> Self {
        Self {
            localhost_profile: profile.localhost_profile,
            type_: profile.type_,
        }
    }
}

impl From<SeccompProfile> for k8s_openapi::api::core::v1::SeccompProfile {
    fn from(profile: SecurityProfile) -> Self {
        Self {
            localhost_profile: profile.localhost_profile,
            type_: profile.type_,
        }
    }
}

/// Configuration options for how the agent performs DNS resolution.
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
#[config(derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct AgentDnsConfig {
    /// ### agent.dns.timeout {#agent-dns-timeout}
    ///
    /// Specifies how long (in seconds) the agent will wait for a DNS response before timing out.
    /// If not specified the agent uses a default value of 1 second.
    /// Setting this too high may cause the internal proxy to time out and exit.
    pub timeout: Option<u32>,

    /// ### agent.dns.attempts {#agent-dns-attempts}
    ///
    /// Specifies the number of DNS resolution attempts the agent will make before failing.
    /// Setting this too high may cause the internal proxy to time out and exit.
    pub attempts: Option<u32>,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::config::{ConfigContext, MirrordConfig};

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

        let mut cfg_context = ConfigContext::default()
            .override_env_opt("MIRRORD_AGENT_RUST_LOG", log_level.0)
            .override_env_opt("MIRRORD_AGENT_NAMESPACE", namespace.0)
            .override_env_opt("MIRRORD_AGENT_IMAGE", image_str)
            .override_env_opt("MIRRORD_AGENT_IMAGE_PULL_POLICY", image_pull_policy.0)
            .override_env_opt("MIRRORD_AGENT_TTL", ttl.0)
            .override_env_opt("MIRRORD_EPHEMERAL_CONTAINER", ephemeral.0)
            .override_env_opt(
                "MIRRORD_AGENT_COMMUNICATION_TIMEOUT",
                communication_timeout.0,
            )
            .override_env_opt("MIRRORD_AGENT_STARTUP_TIMEOUT", startup_timeout.0)
            .strict_env(true);
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
    }
}
