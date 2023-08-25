use std::{collections::HashSet, fmt, str::FromStr};

use bimap::BiMap;
use mirrord_analytics::{AnalyticValue, Analytics, CollectAnalytics};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
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
///         "port_mapping": [[ 7777, 8888 ]],
///         "ignore_localhost": false,
///         "ignore_ports": [9999, 10000]
///         "listen_ports": [[80, 8111]]
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
    Advanced(Box<IncomingAdvancedFileConfig>),
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
                on_concurrent_steal: FromEnv::new("MIRRORD_OPERATOR_ON_CONCURRENT_STEAL")
                    .layer(|layer| {
                        Unstable::new("IncomingFileConfig", "on_concurrent_steal", layer)
                    })
                    .source_value()
                    .transpose()?
                    .unwrap_or_default(),
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
                http_filter: advanced.http_filter.unwrap_or_default().generate_config()?,
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
                listen_ports: advanced
                    .listen_ports
                    .map(|m| m.into_iter().collect())
                    .unwrap_or_default(),
                on_concurrent_steal: FromEnv::new("MIRRORD_OPERATOR_ON_CONCURRENT_STEAL")
                    .or(advanced.on_concurrent_steal)
                    .layer(|layer| {
                        Unstable::new("IncomingFileConfig", "on_concurrent_steal", layer)
                    })
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

        let on_concurrent_steal = FromEnv::new("MIRRORD_OPERATOR_ON_CONCURRENT_STEAL")
            .layer(|layer| Unstable::new("IncomingFileConfig", "on_concurrent_steal", layer))
            .source_value()
            .transpose()?
            .unwrap_or_default();

        Ok(IncomingConfig {
            mode,
            on_concurrent_steal,
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

    /// ### HTTP Filter
    ///
    /// Sets up the HTTP traffic filter (currently, only useful when `incoming: steal`).
    ///
    /// See [`filter`](##filter) for details.
    pub http_filter: Option<ToggleableConfig<http_filter::HttpFilterFileConfig>>,

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

    /// ### listen_ports
    ///
    /// Mapping for local ports to actually used local ports.
    /// When application listens on a port while steal/mirror is active
    /// we fallback to random ports to avoid port conflicts.
    /// Using this configuration will always use the specified port.
    /// If this configuration doesn't exist, mirrord will try to listen on the original port
    /// and if it fails it will assign a random port
    ///
    /// This is useful when you want to access ports exposed by your service locally
    /// For example, if you have a service that listens on port `80` and you want to access it,
    /// you probably can't listen on `80` without sudo, so you can use `[[80, 4480]]`
    /// then access it on `4480` while getting traffic from remote `80`.
    /// The value of `port_mapping` doesn't affect this.
    pub listen_ports: Option<Vec<(u16, u16)>>,
    /// ### on_concurrent_steal
    ///
    /// (Operator Only): if value of override will force close any other connections on requested
    /// target
    pub on_concurrent_steal: Option<ConcurrentSteal>,
}

/// Controls the incoming TCP traffic feature.
///
/// See the incoming [reference](https://mirrord.dev/docs/reference/traffic/#incoming) for more
/// details.
///
/// Incoming traffic supports 3 modes of operation:
///
/// 1. Mirror (**default**): Sniffs the TCP data from a port, and forwards a copy to the interested
/// listeners;
///
/// 2. Steal: Captures the TCP data from a port, and forwards it to the local process, see
/// [`"mode": "steal"`](#feature-network-incoming-mode);
///
/// 3. Off: Disables the incoming network feature.
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
///         "ignore_ports": [9999, 10000],
///         "listen_ports": [[80, 8111]]
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

    /// #### feature.network.incoming.filter {#feature-network-incoming-http-filter}
    pub http_filter: HttpFilterConfig,

    /// #### feature.network.incoming.listen_ports {#feature-network-incoming-listen_ports}
    ///
    /// Mapping for local ports to actually used local ports.
    /// When application listens on a port while steal/mirror is active
    /// we fallback to random ports to avoid port conflicts.
    /// Using this configuration will always use the specified port.
    /// If this configuration doesn't exist, mirrord will try to listen on the original port
    /// and if it fails it will assign a random port
    ///
    /// This is useful when you want to access ports exposed by your service locally
    /// For example, if you have a service that listens on port `80` and you want to access it,
    /// you probably can't listen on `80` without sudo, so you can use `[[80, 4480]]`
    /// then access it on `4480` while getting traffic from remote `80`.
    /// The value of `port_mapping` doesn't affect this.
    pub listen_ports: BiMap<u16, u16>,
    /// #### feature.network.incoming.on_concurrent_steal {#feature-network-incoming-on_concurrent_steal}
    pub on_concurrent_steal: ConcurrentSteal,
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
/// Can be set to either `"mirror"` (default), `"steal"` or `"off"`.
///
/// - `"mirror"`: Sniffs on TCP port, and send a copy of the data to listeners.
/// - `"off"`: Disables the incoming network feature.
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
#[derive(Deserialize, PartialEq, Eq, Clone, Copy, Debug, JsonSchema, Default)]
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

    /// <!--${internal}-->
    /// ### Off
    ///
    /// Disables the incoming network feature.
    Off,
}

#[derive(Error, Debug)]
#[error("could not parse IncomingConfig from string, values must be bool or mirror/steal")]
pub struct IncomingConfigParseError;

impl FromStr for IncomingMode {
    type Err = IncomingConfigParseError;

    fn from_str(val: &str) -> Result<Self, Self::Err> {
        match val.parse::<bool>() {
            Ok(true) => Ok(Self::Steal),
            Ok(false) => Ok(Self::Off),
            Err(_) => match val {
                "steal" => Ok(Self::Steal),
                "mirror" => Ok(Self::Mirror),
                "off" => Ok(Self::Off),
                _ => Err(IncomingConfigParseError),
            },
        }
    }
}

/// (Operator Only): Allows overriding port locks
///
/// Can be set to either `"continue"` or `"override"`.
///
/// - `"continue"`: Continue with normal execution
/// - `"override"`: If port lock detected then override it with new lock and force close the
///   original locking connection.
#[derive(Default, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConcurrentSteal {
    /// <!--${internal}-->
    /// ### override
    ///
    /// Override any port lock and force close the original lock connection
    Override,
    /// <!--${internal}-->
    /// ### continue
    ///
    /// Continue with normal execution
    Continue,
    /// <!--${internal}-->
    /// ### abort
    ///
    /// Abort Execution when trying to steal traffic from a target whose traffic is already being
    /// stolen.
    #[default]
    Abort,
}

#[derive(Error, Debug)]
#[error("could not parse ConcurrentSteal from string, values continue/override")]
pub struct ConcurrentStealParseError;

impl FromStr for ConcurrentSteal {
    type Err = ConcurrentStealParseError;

    fn from_str(val: &str) -> Result<Self, Self::Err> {
        match val {
            "abort" => Ok(Self::Abort),
            "continue" => Ok(Self::Continue),
            "override" => Ok(Self::Override),
            _ => Err(ConcurrentStealParseError),
        }
    }
}

impl fmt::Display for ConcurrentSteal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Abort => write!(f, "abort"),
            Self::Continue => write!(f, "continue"),
            Self::Override => write!(f, "override"),
        }
    }
}

impl From<&IncomingMode> for AnalyticValue {
    fn from(value: &IncomingMode) -> Self {
        match value {
            IncomingMode::Mirror => AnalyticValue::Number(0),
            IncomingMode::Steal => AnalyticValue::Number(1),
            IncomingMode::Off => AnalyticValue::Number(2),
        }
    }
}

impl From<&ConcurrentSteal> for AnalyticValue {
    fn from(value: &ConcurrentSteal) -> Self {
        match value {
            ConcurrentSteal::Override => AnalyticValue::Number(0),
            ConcurrentSteal::Continue => AnalyticValue::Number(1),
            ConcurrentSteal::Abort => AnalyticValue::Number(2),
        }
    }
}

impl CollectAnalytics for &IncomingConfig {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        analytics.add("mode", &self.mode);
        analytics.add("concurrent_steal", &self.on_concurrent_steal);
        analytics.add("port_mapping_count", self.port_mapping.len());
        analytics.add("listen_ports_count", self.listen_ports.len());
        analytics.add("ignore_localhost", self.ignore_localhost);
        analytics.add("ignore_ports_count", self.ignore_ports.len());
        analytics.add("http", &self.http_filter);
    }
}
