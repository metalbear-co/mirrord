use mirrord_config_derive::MirrordConfig;
use mirrord_http::HttpFilter;
use schemars::JsonSchema;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
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
    #[config(env = "MIRRORD_AGENT_TCP_STEAL_TRAFFIC", default)]
    pub incoming: IncomingConfig,

    /// Tunnel outgoing network operations through mirrord.
    #[config(toggleable, nested)]
    pub outgoing: OutgoingConfig,

    /// Resolve DNS via the remote pod.
    #[config(env = "MIRRORD_REMOTE_DNS", default = true)]
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
        let incoming = FromEnv::new("MIRRORD_AGENT_TCP_STEAL_TRAFFIC")
            .source_value()
            .transpose()?
            .unwrap_or(IncomingConfig::Mirror);
        let dns = FromEnv::new("MIRRORD_REMOTE_DNS")
            .source_value()
            .transpose()?
            .unwrap_or(false);
        Ok(NetworkConfig {
            incoming,
            dns,
            outgoing: OutgoingFileConfig::disabled_config()?,
            http_include: FromEnv::new("MIRRORD_HTTP_FILTER_INCLUDE").source_value(),
            http_exclude: FromEnv::new("MIRRORD_HTTP_FILTER_EXCLUDE").source_value(),
        })
    }
}

impl From<NetworkConfig> for HttpFilter {
    /// Initializes a `HttpFilter` based on the user configuration.
    ///
    /// - [`HttpFilter::Include`] is returned if the user specified any include path (thus erasing
    ///   anything passed as exclude);
    /// - [`HttpFilter::Exclude`] also appends the [`DEFAULT_EXCLUDE_LIST`] to the user supplied
    ///   regex;
    #[tracing::instrument(level = "debug")]
    fn from(network_config: NetworkConfig) -> Self {
        let NetworkConfig {
            http_include,
            http_exclude,
            ..
        } = network_config;

        let include = http_include.map(VecOrSingle::to_vec).unwrap_or_default();
        let exclude = http_exclude.map(VecOrSingle::to_vec).unwrap_or_default();

        Self::new(include, exclude)
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
