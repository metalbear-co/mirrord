use std::ops::Deref;

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::filter::ProtocolAndAddressFilter;
use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigContext, ConfigError},
    util::{MirrordToggleableConfig, VecOrSingle},
};

/// List of addresses/ports/subnets that should be sent through either the remote pod or local app,
/// depending how you set this up with either `remote` or `local`.
///
/// You may use this option to specify when outgoing traffic is sent from the remote pod (which
/// is the default behavior when you enable outgoing traffic), or from the local app (default when
/// you have outgoing traffic disabled).
///
/// Takes a list of values, such as:
///
/// - Only UDP traffic on subnet `1.1.1.0/24` on port 1337 will go through the remote pod.
///
/// ```json
/// {
///   "remote": ["udp://1.1.1.0/24:1337"]
/// }
/// ```
///
/// - Only UDP and TCP traffic on resolved address of `google.com` on port `1337` and `7331` will go
///   through the remote pod.
/// ```json
/// {
///   "remote": ["google.com:1337", "google.com:7331"]
/// }
/// ```
///
/// - Only TCP traffic on `localhost` on port 1337 will go through the local app, the rest will be
///   emmited remotely in the cluster.
///
/// ```json
/// {
///   "local": ["tcp://localhost:1337"]
/// }
/// ```
///
/// - Only outgoing traffic on port `1337` and `7331` will go through the local app.
/// ```json
/// {
///   "local": [":1337", ":7331"]
/// }
/// ```
///
/// Valid values follow this pattern: `[protocol]://[name|address|subnet/mask]:[port]`.
#[derive(Serialize, Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum OutgoingFilterConfig {
    /// Traffic that matches what's specified here will go through the remote pod, everything else
    /// will go through local.
    Remote(VecOrSingle<String>),

    /// Traffic that matches what's specified here will go through the local app, everything else
    /// will go through the remote pod.
    Local(VecOrSingle<String>),
}

/// Tunnel outgoing network operations through mirrord.
///
/// See the outgoing [reference](https://mirrord.dev/docs/reference/traffic/#outgoing) for more
/// details.
///
/// The `remote` and `local` config for this feature are **mutually** exclusive.
///
/// ```json
/// {
///   "feature": {
///     "network": {
///       "outgoing": {
///         "tcp": true,
///         "udp": true,
///         "ignore_localhost": false,
///         "filter": {
///           "local": ["tcp://1.1.1.0/24:1337", "1.1.5.0/24", "google.com", ":53"]
///         },
///         "unix_streams": "bear.+"
///       }
///     }
///   }
/// }
/// ```
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
#[config(map_to = "OutgoingFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct OutgoingConfig {
    /// #### feature.network.outgoing.tcp {#feature.network.outgoing.tcp}
    ///
    /// Defaults to `true`.
    #[config(env = "MIRRORD_TCP_OUTGOING", default = true)]
    pub tcp: bool,

    /// #### feature.network.outgoing.udp {#feature.network.outgoing.udp}
    ///
    /// Defaults to `true`.
    #[config(env = "MIRRORD_UDP_OUTGOING", default = true)]
    pub udp: bool,

    /// #### feature.network.outgoing.ignore_localhost {#feature.network.outgoing.ignore_localhost}
    ///
    /// Defaults to `false`.
    // Consider removing when adding https://github.com/metalbear-co/mirrord/issues/702
    #[config(default = false)]
    pub ignore_localhost: bool,

    /// #### feature.network.outgoing.filter {#feature.network.outgoing.filter}
    ///
    /// Filters that are used to send specific traffic from either the remote pod or the local app
    #[config(default)]
    pub filter: Option<OutgoingFilterConfig>,

    /// #### feature.network.outgoing.unix_streams {#feature.network.outgoing.unix_streams}
    ///
    /// Connect to these unix streams remotely (and to all other paths locally).
    ///
    /// You can either specify a single value or an array of values.
    /// Each value is interpreted as a regular expression
    /// ([Supported Syntax](https://docs.rs/regex/1.7.1/regex/index.html#syntax)).
    ///
    /// When your application connects to a unix socket, the target address will be converted to a
    /// string (non-utf8 bytes are replaced by a placeholder character) and matched against the set
    /// of regexes specified here. If there is a match, mirrord will connect your application with
    /// the target unix socket address on the target pod. Otherwise, it will leave the connection
    /// to happen locally on your machine.
    #[config(unstable, env = "MIRRORD_OUTGOING_REMOTE_UNIX_STREAMS")]
    pub unix_streams: Option<VecOrSingle<String>>,
}

impl MirrordToggleableConfig for OutgoingFileConfig {
    fn disabled_config(context: &mut ConfigContext) -> Result<Self::Generated, ConfigError> {
        Ok(OutgoingConfig {
            tcp: FromEnv::new("MIRRORD_TCP_OUTGOING")
                .source_value(context)
                .unwrap_or(Ok(false))?,
            udp: FromEnv::new("MIRRORD_UDP_OUTGOING")
                .source_value(context)
                .unwrap_or(Ok(false))?,
            unix_streams: FromEnv::new("MIRRORD_OUTGOING_REMOTE_UNIX_STREAMS")
                .source_value(context)
                .transpose()?,
            ..Default::default()
        })
    }
}

impl CollectAnalytics for &OutgoingConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("tcp", self.tcp);
        analytics.add("udp", self.udp);
        analytics.add("ignore_localhost", self.ignore_localhost);
        analytics.add(
            "unix_streams",
            self.unix_streams
                .as_ref()
                .map(|v| v.len())
                .unwrap_or_default(),
        );

        if let Some(filter) = self.filter.as_ref() {
            match filter {
                OutgoingFilterConfig::Remote(value) => {
                    analytics.add("outgoing_filter_remote", value.len())
                }
                OutgoingFilterConfig::Local(value) => {
                    analytics.add("outgoing_filter_local", value.len())
                }
            }
        }
    }
}

impl OutgoingConfig {
    pub fn verify(&self, _: &mut ConfigContext) -> Result<(), ConfigError> {
        let filters = match self.filter.as_ref() {
            None => return Ok(()),
            Some(OutgoingFilterConfig::Local(filters)) => filters.deref(),
            Some(OutgoingFilterConfig::Remote(filters)) => filters.deref(),
        };

        for filter in filters {
            let Err(error) = filter.parse::<ProtocolAndAddressFilter>() else {
                continue;
            };

            return Err(ConfigError::InvalidValue {
                name: "feature.network.outgoing.filter",
                provided: filter.to_string(),
                error: Box::new(error),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use crate::{
        config::{ConfigContext, MirrordConfig},
        feature::network::OutgoingFileConfig,
        util::ToggleableConfig,
    };

    #[rstest]
    fn default(
        #[values((None, true), (Some("false"), false), (Some("true"), true))] tcp: (
            Option<&str>,
            bool,
        ),
        #[values((None, true), (Some("false"), false), (Some("true"), true))] udp: (
            Option<&str>,
            bool,
        ),
    ) {
        let mut cfg_context = ConfigContext::default()
            .override_env_opt("MIRRORD_TCP_OUTGOING", tcp.0)
            .override_env_opt("MIRRORD_UDP_OUTGOING", udp.0)
            .strict_env(true);
        let outgoing = OutgoingFileConfig::default()
            .generate_config(&mut cfg_context)
            .unwrap();

        assert_eq!(outgoing.tcp, tcp.1);
        assert_eq!(outgoing.udp, udp.1);
    }

    #[rstest]
    fn disabled(
        #[values((None, false), (Some("false"), false), (Some("true"), true))] tcp: (
            Option<&str>,
            bool,
        ),
        #[values((None, false), (Some("false"), false), (Some("true"), true))] udp: (
            Option<&str>,
            bool,
        ),
    ) {
        let mut cfg_context = ConfigContext::default()
            .override_env_opt("MIRRORD_TCP_OUTGOING", tcp.0)
            .override_env_opt("MIRRORD_UDP_OUTGOING", udp.0)
            .strict_env(true);
        let outgoing = ToggleableConfig::<OutgoingFileConfig>::Enabled(false)
            .generate_config(&mut cfg_context)
            .unwrap();

        assert_eq!(outgoing.tcp, tcp.1);
        assert_eq!(outgoing.udp, udp.1);
    }
}
