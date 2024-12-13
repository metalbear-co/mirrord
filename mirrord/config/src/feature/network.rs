use dns::{DnsConfig, DnsFileConfig};
use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::Serialize;

use self::{incoming::*, outgoing::*};
use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigContext, ConfigError},
    util::MirrordToggleableConfig,
};

const IPV6_ENV_VAR: &str = "MIRRORD_INCOMING_ENABLE_IPV6";

pub mod dns;
pub mod filter;
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
///         "http_filter": {
///           "header_filter": "host: api\\..+"
///         },
///         "port_mapping": [[ 7777, 8888 ]],
///         "ignore_localhost": false,
///         "ignore_ports": [9999, 10000]
///       },
///       "outgoing": {
///         "tcp": true,
///         "udp": true,
///         "filter": {
///           "local": ["tcp://1.1.1.0/24:1337", "1.1.5.0/24", "google.com", ":53"]
///         },
///         "ignore_localhost": false,
///         "unix_streams": "bear.+"
///       },
///       "dns": {
///         "enabled": true,
///         "filter": {
///           "local": ["1.1.1.0/24:1337", "1.1.5.0/24", "google.com"]
///         }
///       }
///     }
///   }
/// }
/// ```
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug, Serialize)]
#[config(map_to = "NetworkFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct NetworkConfig {
    /// ### feature.network.incoming {#feature-network-incoming}
    #[config(toggleable, nested)]
    pub incoming: IncomingConfig,

    /// ### feature.network.outgoing {#feature-network-outgoing}
    #[config(toggleable, nested)]
    pub outgoing: OutgoingConfig,

    /// ### feature.network.dns {#feature-network-dns}
    #[config(toggleable, nested)]
    pub dns: DnsConfig,

    /// ### feature.network.ipv6 {#feature-network-dns}
    ///
    /// Enable ipv6 support. Turn on if your application listens to incoming traffic over IPv6.
    #[config(env = IPV6_ENV_VAR)]
    pub ipv6: bool,
}

impl MirrordToggleableConfig for NetworkFileConfig {
    fn disabled_config(context: &mut ConfigContext) -> Result<Self::Generated, ConfigError> {
        let ipv6 = FromEnv::new(IPV6_ENV_VAR)
            .source_value(context)
            .transpose()?
            .unwrap_or_default();

        Ok(NetworkConfig {
            incoming: IncomingFileConfig::disabled_config(context)?,
            dns: DnsFileConfig::disabled_config(context)?,
            outgoing: OutgoingFileConfig::disabled_config(context)?,
            ipv6,
        })
    }
}

impl CollectAnalytics for &NetworkConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("incoming", &self.incoming);
        analytics.add("outgoing", &self.outgoing);
        analytics.add("dns", &self.dns);
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
                IncomingConfig { mode: IncomingMode::Off, ..Default::default() }
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
                let mut cfg_context = ConfigContext::default();
                let env = NetworkFileConfig::default()
                    .generate_config(&mut cfg_context)
                    .unwrap();

                assert_eq!(env.incoming, incoming.1);
                assert_eq!(env.dns.enabled, dns.1);
            },
        );
    }
}
