use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use crate::{
    config::{
        default_value::DefaultValue, from_env::FromEnv, source::MirrordConfigSource, ConfigError,
    },
    incoming::IncomingConfig,
    outgoing::{OutgoingConfig, OutgoingFileConfig},
    util::{MirrordToggleableConfig, VecOrSingle},
};

/// Controls mirrord network operations.
///
/// See the network traffic [reference](https://mirrord.dev/docs/reference/traffic/)
/// for more details.
///
/// ## Examples
///
/// - Steal incoming traffic, enable TCP outgoing traffic and DNS resolution:
///
/// ```toml
/// # mirrord-config.toml
///
/// [feature.network]
/// incoming = "steal"
/// dns = true # not needed, as this is the default
///
/// [feature.network.outgoing]
/// tcp = true
/// ```
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "NetworkFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct NetworkConfig {
    /// Mode of operation for incoming network requests in mirrord, supports `mirror` or `steal`:
    ///
    /// - `mirror`: mirror incoming requests to the remote pod to the local process;
    /// - `steal`: redirect incoming requests to the remote pod to the local process
    #[config(env = "MIRRORD_AGENT_TCP_STEAL_TRAFFIC", default = "mirror")]
    pub incoming: IncomingConfig,

    /// Tunnel outgoing network operations through mirrord.
    #[config(toggleable, nested)]
    pub outgoing: OutgoingConfig,

    /// Resolve DNS via the remote pod.
    #[config(env = "MIRRORD_REMOTE_DNS", default = "true")]
    pub dns: bool,

    // TODO(alex) [mid] 2022-11-17: Improve these docs.
    /// Allows the user to specify regexes that are used to match against HTTP headers when mirrord
    /// network operations are enabled.
    ///
    /// The regexes specified here will make mirrord operate only on requests that match it,
    /// otherwise the request will not be stolen.
    #[config(env = "MIRRORD_HTTP_FILTER_INCLUDE")]
    pub http_include: Option<VecOrSingle<String>>,

    /// Allows the user to specify regexes that are used to match against HTTP headers when mirrord
    /// network operations are enabled.
    ///
    /// The opposite of `include`, requests that match the regexes specified here will bypass
    /// mirrord (won't be stolen).
    #[config(env = "MIRRORD_HTTP_FILTER_EXCLUDE")]
    pub http_exclude: Option<VecOrSingle<String>>,
}

impl MirrordToggleableConfig for NetworkFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(NetworkConfig {
            incoming: (
                FromEnv::new("MIRRORD_AGENT_TCP_STEAL_TRAFFIC"),
                DefaultValue::new("mirror"),
            )
                .source_value()
                .ok_or(ConfigError::ValueNotProvided(
                    "NetworkFileConfig",
                    "incoming",
                    Some("MIRRORD_AGENT_TCP_STEAL_TRAFFIC"),
                ))?,
            dns: (
                FromEnv::new("MIRRORD_REMOTE_DNS"),
                DefaultValue::new("false"),
            )
                .source_value()
                .ok_or(ConfigError::ValueNotProvided(
                    "NetworkFileConfig",
                    "dns",
                    Some("MIRRORD_REMOTE_DNS"),
                ))?,
            outgoing: OutgoingFileConfig::disabled_config()?,
            http_include: FromEnv::new("MIRRORD_HTTP_FILTER_INCLUDE").source_value(),
            http_exclude: FromEnv::new("MIRRORD_HTTP_FILTER_EXCLUDE").source_value(),
        })
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{config::MirrordConfig, util::testing::with_env_vars};

    #[rstest]
    fn default(
        #[values((None, IncomingConfig::Mirror), (Some("false"), IncomingConfig::Mirror), (Some("true"), IncomingConfig::Steal))]
        incoming: (Option<&str>, IncomingConfig),
        #[values((None, true), (Some("false"), false))] dns: (Option<&str>, bool),
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_AGENT_TCP_STEAL_TRAFFIC", incoming.0),
                ("MIRRORD_REMOTE_DNS", dns.0),
            ],
            || {
                let env = NetworkFileConfig::default().generate_config().unwrap();

                assert_eq!(env.incoming, incoming.1);
                assert_eq!(env.dns, dns.1);
            },
        );
    }
}
