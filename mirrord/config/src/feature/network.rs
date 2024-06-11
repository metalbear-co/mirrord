use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;

use self::{incoming::*, outgoing::*};
use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigContext, ConfigError},
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

    /// ### feature.network.outgoing {#feature-network-outgoing}
    #[config(toggleable, nested)]
    pub outgoing: OutgoingConfig,

    /// ### feature.network.dns {#feature-network-dns}
    ///
    /// Resolve DNS via the remote pod.
    ///
    /// Defaults to `true`.
    ///
    /// - Caveats: DNS resolving can be done in multiple ways, some frameworks will use
    /// `getaddrinfo`, while others will create a connection on port `53` and perform a sort
    /// of manual resolution. Just enabling the `dns` feature in mirrord might not be enough.
    /// If you see an address resolution error, try enabling the [`fs`](#feature-fs) feature,
    /// and setting `read_only: ["/etc/resolv.conf"]`.
    #[config(env = "MIRRORD_REMOTE_DNS", default = true)]
    pub dns: bool,
}

impl MirrordToggleableConfig for NetworkFileConfig {
    fn disabled_config(context: &mut ConfigContext) -> Result<Self::Generated, ConfigError> {
        let dns = FromEnv::new("MIRRORD_REMOTE_DNS")
            .source_value(context)
            .transpose()?
            .unwrap_or(false);

        Ok(NetworkConfig {
            incoming: IncomingFileConfig::disabled_config(context)?,
            dns,
            outgoing: OutgoingFileConfig::disabled_config(context)?,
        })
    }
}

impl CollectAnalytics for &NetworkConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("incoming", &self.incoming);
        analytics.add("outgoing", &self.outgoing);
        analytics.add("dns", self.dns);
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
                assert_eq!(env.dns, dns.1);
            },
        );
    }
}
