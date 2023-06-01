use std::{collections::HashSet, str::FromStr};

use bimap::BiMap;
use schemars::JsonSchema;
use serde::Deserialize;
use thiserror::Error;

use crate::{
    config::{
        from_env::FromEnv, source::MirrordConfigSource, unstable::Unstable, ConfigError,
        FromMirrordConfig, MirrordConfig, Result,
    },
    util::{MirrordToggleableConfig, ToggleableConfig},
};

pub mod http_filter;

use http_filter::*;

/// ## incoming (network)
///
/// Controls the incoming TCP traffic feature.
///
/// See the incoming [reference](https://mirrord.dev/docs/reference/traffic/#incoming) for more
/// details.
///
/// Incoming traffic supports 2 modes of operation:
///
/// 1. Mirror (**default**): Sniffs the TCP data from a port, and forwards a copy to the interested
/// listeners;
///
/// 2. Steal: Captures the TCP data from a port, and forwards it to the local process, see
/// [`steal`](##steal);
///
/// ### Minimal `incoming` config
///
/// ```json
/// {
///   "feature": {
///     "network": {
///       "incoming": "steal"
///     }
///   }
/// }
/// ```
///
/// ### Advanced `incoming` config
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
///         "port_mapping": [[ 7777: 8888 ]],
///         "ignore_localhost": false,
///         "ignore_ports": [9999, 10000]
///       }
///     }
///   }
/// }
/// ```
#[derive(Deserialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(PartialEq, Eq))]
#[serde(untagged, rename_all = "lowercase")]
pub enum IncomingFileConfig {
    Simple(Option<IncomingMode>),
    Advanced(IncomingAdvancedFileConfig),
}

impl Default for IncomingFileConfig {
    fn default() -> Self {
        IncomingFileConfig::Simple(None)
    }
}

impl FromMirrordConfig for IncomingConfig {
    type Generator = IncomingFileConfig;
}

impl MirrordConfig for IncomingFileConfig {
    type Generated = IncomingConfig;

    fn generate_config(self) -> Result<Self::Generated> {
        let config = match self {
            IncomingFileConfig::Simple(mode) => IncomingConfig {
                mode: FromEnv::new("MIRRORD_AGENT_TCP_STEAL_TRAFFIC")
                    .or(mode)
                    .source_value()
                    .transpose()?
                    .unwrap_or_default(),
                http_header_filter: HttpHeaderFilterFileConfig::default().generate_config()?,
                ..Default::default()
            },
            IncomingFileConfig::Advanced(advanced) => IncomingConfig {
                mode: FromEnv::new("MIRRORD_AGENT_TCP_STEAL_TRAFFIC")
                    .or(advanced.mode)
                    .source_value()
                    .transpose()?
                    .unwrap_or_default(),
                http_header_filter: advanced
                    .http_header_filter
                    .unwrap_or_default()
                    .generate_config()?,
                port_mapping: advanced
                    .port_mapping
                    .map(|m| m.into_iter().collect())
                    .unwrap_or_default(),
                ignore_ports: advanced
                    .ignore_ports
                    .map(|m| m.into_iter().collect())
                    .unwrap_or_default(),
                ignore_localhost: advanced
                    .ignore_localhost
                    .layer(|layer| Unstable::new("IncomingFileConfig", "ignore_localhost", layer))
                    .source_value()
                    .transpose()?
                    .unwrap_or_default(),
            },
        };

        Ok(config)
    }
}
impl MirrordToggleableConfig for IncomingFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let mode = FromEnv::new("MIRRORD_AGENT_TCP_STEAL_TRAFFIC")
            .source_value()
            .unwrap_or_else(|| Ok(Default::default()))?;

        Ok(IncomingConfig {
            mode,
            http_header_filter: HttpHeaderFilterFileConfig::disabled_config()?,
            ..Default::default()
        })
    }
}

/// ## incoming (advanced setup)
///
/// Advanced user configuration for network incoming traffic.
#[derive(Deserialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct IncomingAdvancedFileConfig {
    /// ### mode
    ///
    /// Allows selecting between mirrorring or stealing traffic.
    ///
    /// See [`mode`](##mode (incoming)) for details.
    pub mode: Option<IncomingMode>,

    /// ### filter
    ///
    /// Sets up the HTTP traffic filter (currently, only useful when `incoming: steal`).
    ///
    /// See [`filter`](##filter) for details.
    pub http_header_filter: Option<ToggleableConfig<http_filter::HttpHeaderFilterFileConfig>>,

    /// ### port_mapping
    ///
    /// Mapping for local ports to remote ports.
    ///
    /// This is useful when you want to mirror/steal a port to a different port on the remote
    /// machine. For example, your local process listens on port `9333` and the container listens
    /// on port `80`. You'd use `[[9333, 80]]`
    pub port_mapping: Option<Vec<(u16, u16)>>,

    /// ### ignore_localhost
    ///
    /// Consider removing when adding https://github.com/metalbear-co/mirrord/issues/702
    pub ignore_localhost: Option<bool>,

    /// ### ignore_ports
    ///
    /// Ports to ignore when mirroring/stealing traffic. Useful if you want specific ports to be
    /// used locally only.
    pub ignore_ports: Option<Vec<u16>>,
}

/// Controls the incoming TCP traffic feature.
///
/// See the incoming [reference](https://mirrord.dev/docs/reference/traffic/#incoming) for more
/// details.
///
/// Incoming traffic supports 2 modes of operation:
///
/// 1. Mirror (**default**): Sniffs the TCP data from a port, and forwards a copy to the interested
/// listeners;
///
/// 2. Steal: Captures the TCP data from a port, and forwards it to the local process, see
/// [`"mode": "steal"`](#feature-network-incoming-mode);
///
/// Steals all the incoming traffic:
///
/// ```json
/// {
///   "feature": {
///     "network": {
///       "incoming": "steal"
///     }
///   }
/// }
/// ```
///
/// Steals only traffic that matches the
/// [`http_header_filter`](#feature-network-incoming-http_header_filter) (steals only HTTP traffic).
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
///       }
///     }
///   }
/// }
/// ```
#[derive(Default, PartialEq, Eq, Clone, Debug)]
pub struct IncomingConfig {
    /// #### feature.network.incoming.port_mapping {#feature-network-incoming-port_mapping}
    ///
    /// Mapping for local ports to remote ports.
    ///
    /// This is useful when you want to mirror/steal a port to a different port on the remote
    /// machine. For example, your local process listens on port `9333` and the container listens
    /// on port `80`. You'd use `[[9333, 80]]`
    pub port_mapping: BiMap<u16, u16>,

    /// #### feature.network.incoming.ignore_localhost {#feature-network-incoming-ignore_localhost}
    pub ignore_localhost: bool,

    /// #### feature.network.incoming.ignore_ports {#feature-network-incoming-ignore_ports}
    ///
    /// Ports to ignore when mirroring/stealing traffic, these ports will remain local.
    ///
    /// Can be especially useful when
    /// [`feature.network.incoming.mode`](#feature-network-incoming-mode) is set to `"stealer"
    /// `, and you want to avoid redirecting traffic from some ports (for example, traffic from
    /// a health probe, or other heartbeat-like traffic).
    pub ignore_ports: HashSet<u16>,

    /// #### feature.network.incoming.mode {#feature-network-incoming-mode}
    pub mode: IncomingMode,

    /// #### feature.network.incoming.filter {#feature-network-incoming-filter}
    pub http_header_filter: HttpHeaderFilterConfig,
}

impl IncomingConfig {
    /// <!--${internal}-->
    /// Helper function.
    ///
    /// Used by mirrord-layer to identify the incoming network configuration as steal or not.
    pub fn is_steal(&self) -> bool {
        matches!(self.mode, IncomingMode::Steal)
    }
}

/// Allows selecting between mirrorring or stealing traffic.
///
/// Can be set to either `"mirror"` (default) or `"steal"`.
///
/// - `"mirror"`: Sniffs on TCP port, and send a copy of the data to listeners.
/// - `"steal"`: Supports 2 modes of operation:
///
/// 1. Port traffic stealing: Steals all TCP data from a
///   port, which is selected whenever the
/// user listens in a TCP socket (enabling the feature is enough to make this work, no
/// additional configuration is needed);
///
/// 2. HTTP traffic stealing: Steals only HTTP traffic, mirrord tries to detect if the incoming
/// data on a port is HTTP (in a best-effort kind of way, not guaranteed to be HTTP), and
/// steals the traffic on the port if it is HTTP;
#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum IncomingMode {
    /// <!--${internal}-->
    /// ### mirror
    ///
    /// Sniffs on TCP port, and send a copy of the data to listeners.
    #[default]
    Mirror,

    /// <!--${internal}-->
    /// ### steal
    ///
    /// Stealer supports 2 modes of operation:
    ///
    /// 1. Port traffic stealing: Steals all TCP data from a port, which is selected whenever the
    /// user listens in a TCP socket (enabling the feature is enough to make this work, no
    /// additional configuration is needed);
    ///
    /// 2. HTTP traffic stealing: Steals only HTTP traffic, mirrord tries to detect if the incoming
    /// data on a port is HTTP (in a best-effort kind of way, not guaranteed to be HTTP), and
    /// steals the traffic on the port if it is HTTP;
    Steal,
}

#[derive(Error, Debug)]
#[error("could not parse IncomingConfig from string, values must be bool or mirror/steal")]
pub struct IncomingConfigParseError;

impl FromStr for IncomingMode {
    type Err = IncomingConfigParseError;

    fn from_str(val: &str) -> Result<Self, Self::Err> {
        match val.parse::<bool>() {
            Ok(true) => Ok(Self::Steal),
            Ok(false) => Ok(Self::Mirror),
            Err(_) => match val {
                "steal" => Ok(Self::Steal),
                "mirror" => Ok(Self::Mirror),
                _ => Err(IncomingConfigParseError),
            },
        }
    }
}
