use std::collections::HashMap;

use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use crate::config::source::MirrordConfigSource;

/// # `agent`
///
/// Configuration for the mirrord-agent pod that is spawned in the Kubernetes cluster.
///
/// ## Sample
///
/// ```json
/// {
///   "agent": {
////    "log_level": String | "trace" | "debug" | "info" | "warn" | "error",
////    "namespace": null | String,
////    "image": null | String,
////    "image_pull_policy": String,
////    "image_pull_secrets": null | [] | [ { String: String } ],
////    "ttl": Number,
////    "ephemeral": bool,
////    "communication_timeout": null | Number,
////    "startup_timeout": Number,
////    "network_interface": null | String,
////    "pause": bool,
////    "flush_connections": bool,
///   }
/// }
/// ```
/// 
/// ## Examples
///
/// - Basic mirrord-agent configuration:
/// ```json
/// {
///   "agent": {
////    "log_level": "error",
///   }
/// }
/// ```
/// 
/// - Using a custom image:
/// ```json
/// {
///   "agent": {
////    "log_level": "debug",
////    "image": "user-repo/mirrord-agent:latest",
////    "image_pull_policy": "Always",
////    "ttl": 60,
////    "flush_connections": bool,
///   }
/// }
/// ```
///
#[derive(MirrordConfig, Clone, Debug)]
#[config(map_to = "AgentFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct AgentConfig {
    /// # log_level
    ///
    /// Log level for the agent.
    ///
    /// Supports anything that would work with `RUST_LOG`.
    #[config(env = "MIRRORD_AGENT_RUST_LOG", default = "info")]
    pub log_level: String,

    /// # namespace
    ///
    /// Namespace where the agent shall live.
    ///
    /// Defaults to the current kubernetes namespace.
    #[config(env = "MIRRORD_AGENT_NAMESPACE")]
    pub namespace: Option<String>,

    /// # image
    ///
    /// Name of the agent's docker image.
    ///
    /// Useful when a custom build of mirrord-agent is required, or when using an internal
    /// registry.
    ///
    /// Defaults to the latest stable image.
    #[config(env = "MIRRORD_AGENT_IMAGE")]
    pub image: Option<String>,

    /// # image_pull_policy
    ///
    /// Controls when a new agent image is downloaded.
    ///
    /// Supports any valid kubernetes [image pull
    /// policy](https://kubernetes.io/docs/concepts/containers/images/#image-pull-policy)
    #[config(env = "MIRRORD_AGENT_IMAGE_PULL_POLICY", default = "IfNotPresent")]
    pub image_pull_policy: String,

    /// # image_pull_secrets
    ///
    /// List of secrets the agent pod has access to.
    ///
    /// Takes an array of hash with the format `{ name: <secret-name> }`. Read more [here](https://kubernetes.io/docs/concepts/containers/images/).
    pub image_pull_secrets: Option<Vec<HashMap<String, String>>>,

    /// # ttl
    ///
    /// Controls how long the agent pod persists for after the agent exits (in seconds).
    ///
    /// Can be useful for collecting logs.
    #[config(env = "MIRRORD_AGENT_TTL", default = 1)]
    pub ttl: u16,

    /// # ephemeral
    ///
    /// Runs the agent as an [ephemeral
    /// container](https://kubernetes.io/docs/concepts/workloads/pods/ephemeral-containers/)
    #[config(env = "MIRRORD_EPHEMERAL_CONTAINER", default = false)]
    pub ephemeral: bool,

    /// # communication_timeout
    ///
    /// Controls how long the agent lives when there are no connections.
    ///
    /// Each connection has its own heartbeat mechanism, so even if the local application has no
    /// messages, the agent stays alive until there are no more heartbeat messages.
    #[config(env = "MIRRORD_AGENT_COMMUNICATION_TIMEOUT")]
    pub communication_timeout: Option<u16>,

    /// # startup_timeout
    ///
    /// Controls how long to wait for the agent to finish initialization.
    ///
    /// If initialization takes longer than this value, mirrord exits.
    #[config(env = "MIRRORD_AGENT_STARTUP_TIMEOUT", default = 60)]
    pub startup_timeout: u64,

    /// # network_interface
    ///
    /// Which network interface to use for mirroring.
    ///
    /// The default behavior is try to access the internet and use that interface
    /// and if that fails it uses eth0.
    #[config(env = "MIRRORD_AGENT_NETWORK_INTERFACE")]
    pub network_interface: Option<String>,

    /// # pause
    ///
    /// Controls target pause feature. Unstable.
    ///
    /// With this feature enabled, the remote container is paused while clients are connected to
    /// the agent.
    #[config(env = "MIRRORD_PAUSE", default = false, unstable)]
    pub pause: bool,

    /// # flush_connections
    ///
    /// Flushes existing connections when starting to steal, might fix issues where connections
    /// aren't stolen (due to being already established)
    ///
    /// Temporary fix for issue [#1029](https://github.com/metalbear-co/mirrord/issues/1029).
    #[config(
        env = "MIRRORD_AGENT_STEALER_FLUSH_CONNECTIONS",
        default = true,
        unstable
    )]
    pub flush_connections: bool,

    /// Create an agent that returns an error after accepting the first client. For testing
    /// purposes. Only supported with job agents (not with ephemeral agents).
    #[cfg(all(debug_assertions, not(test)))] // not(test) so that it's not included in the schema json.
    #[config(env = "MIRRORD_AGENT_TEST_ERROR", default = false, unstable)]
    pub test_error: bool,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{config::MirrordConfig, util::testing::with_env_vars};

    #[rstest]
    fn default(
        #[values((None, "info"), (Some("trace"), "trace"))] log_level: (Option<&str>, &str),
        #[values((None, None), (Some("app"), Some("app")))] namespace: (Option<&str>, Option<&str>),
        #[values((None, None), (Some("test"), Some("test")))] image: (Option<&str>, Option<&str>),
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
        with_env_vars(
            vec![
                ("MIRRORD_AGENT_RUST_LOG", log_level.0),
                ("MIRRORD_AGENT_NAMESPACE", namespace.0),
                ("MIRRORD_AGENT_IMAGE", image.0),
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
                let agent = AgentFileConfig::default().generate_config().unwrap();

                assert_eq!(agent.log_level, log_level.1);
                assert_eq!(agent.namespace.as_deref(), namespace.1);
                assert_eq!(agent.image.as_deref(), image.1);
                assert_eq!(agent.image_pull_policy, image_pull_policy.1);
                assert_eq!(agent.ttl, ttl.1);
                assert_eq!(agent.ephemeral, ephemeral.1);
                assert_eq!(agent.communication_timeout, communication_timeout.1);
                assert_eq!(agent.startup_timeout, startup_timeout.1);
            },
        );
    }
}
