use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use self::{incoming::*, outgoing::*};
use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    util::MirrordToggleableConfig,
};

pub mod incoming;
pub mod outgoing;

/// Controls mirrord network operations.
///
/// See the network traffic [reference](https://mirrord.dev/docs/reference/traffic/)
/// for more details.
///
/// ```json
/// {
///   "feature": {
///     "network": {
///       "incoming": {
///         "mode": "steal",
///         "http_header_filter": {
///           "filter": "host: api\..+",
///           "ports": [80, 8080]
///         },
///         "port_mapping": [[ 7777, 8888 ]],
///         "ignore_localhost": false,
///         "ignore_ports": [9999, 10000]
///       },
///       "outgoing": {
///         "tcp": true,
///         "udp": true,
///         "ignore_localhost": false,
///         "unix_streams": "bear.+"
///       },
///       "dns": false
///     }
///   }
/// }
/// ```
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "NetworkFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct NetworkConfig {
    /// ### feature.network.incoming {#feature-network-incoming}
    #[config(toggleable, nested)]
    pub incoming: IncomingConfig,

    /// ### outgoing
    ///
    /// Tunnel outgoing network operations through mirrord, see [`outgoing`](##outgoing) for
    /// more details.
    #[config(toggleable, nested)]
    pub outgoing: OutgoingConfig,

    /// ### dns
    ///
    /// Resolve DNS via the remote pod.
    ///
    /// Defaults to `true`.
    #[config(env = "MIRRORD_REMOTE_DNS", default = true)]
    pub dns: bool,
}

impl MirrordToggleableConfig for NetworkFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let dns = FromEnv::new("MIRRORD_REMOTE_DNS")
            .source_value()
            .transpose()?
            .unwrap_or(false);

        Ok(NetworkConfig {
            incoming: IncomingFileConfig::disabled_config()?,
            dns,
            outgoing: OutgoingFileConfig::disabled_config()?,
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
        #[values(
            (
                None,
                IncomingConfig { mode: IncomingMode::Mirror, ..Default::default() }
            ),
            (
                Some("false"),
                IncomingConfig { mode: IncomingMode::Mirror, ..Default::default() }
            ),
            (
                Some("true"),
                IncomingConfig { mode: IncomingMode::Steal, ..Default::default() }
            ),
        )]
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
