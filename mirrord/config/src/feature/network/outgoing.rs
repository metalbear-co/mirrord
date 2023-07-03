use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    util::{MirrordToggleableConfig, VecOrSingle},
};

/// Tunnel outgoing network operations through mirrord.
///
/// See the outgoing [reference](https://mirrord.dev/docs/reference/traffic/#outgoing) for more
/// details.
///
/// ```json
/// {
///   "feature": {
///     "network": {
///       "outgoing": {
///         "tcp": true,
///         "udp": true,
///         "ignore_localhost": false,
///         "unix_streams": "bear.+"
///       }
///     }
///   }
/// }
/// ```
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
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
    #[config(unstable, default = false)]
    pub ignore_localhost: bool,

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
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(OutgoingConfig {
            tcp: FromEnv::new("MIRRORD_TCP_OUTGOING")
                .source_value()
                .unwrap_or(Ok(false))?,
            udp: FromEnv::new("MIRRORD_UDP_OUTGOING")
                .source_value()
                .unwrap_or(Ok(false))?,
            unix_streams: FromEnv::new("MIRRORD_OUTGOING_REMOTE_UNIX_STREAMS")
                .source_value()
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
    }
}
#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        config::MirrordConfig,
        util::{testing::with_env_vars, ToggleableConfig},
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
        with_env_vars(
            vec![
                ("MIRRORD_TCP_OUTGOING", tcp.0),
                ("MIRRORD_UDP_OUTGOING", udp.0),
            ],
            || {
                let outgoing = OutgoingFileConfig::default().generate_config().unwrap();

                assert_eq!(outgoing.tcp, tcp.1);
                assert_eq!(outgoing.udp, udp.1);
            },
        );
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
        with_env_vars(
            vec![
                ("MIRRORD_TCP_OUTGOING", tcp.0),
                ("MIRRORD_UDP_OUTGOING", udp.0),
            ],
            || {
                let outgoing = ToggleableConfig::<OutgoingFileConfig>::Enabled(false)
                    .generate_config()
                    .unwrap();

                assert_eq!(outgoing.tcp, tcp.1);
                assert_eq!(outgoing.udp, udp.1);
            },
        );
    }
}
