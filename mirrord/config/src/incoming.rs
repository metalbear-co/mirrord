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

/// # incoming
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
/// 2. Steal: Captures the TCP data from a port, and forwards it (depending on how it's configured,
/// see [`IncomingMode::Steal`]);
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

#[derive(Deserialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct IncomingAdvancedFileConfig {
    /// Allows selecting between mirrorring or stealing traffic.
    ///
    /// See [`IncomingMode`] for details.
    pub mode: Option<IncomingMode>,

    /// Sets up the HTTP traffic filter (currently, only for [`IncomingMode::Steal`]).
    ///
    /// See [`HttpHeaderFilterConfig`] for details.
    pub http_header_filter: Option<ToggleableConfig<http_filter::HttpHeaderFilterFileConfig>>,

    /// Mapping for local ports to remote ports.
    ///
    /// This is useful when you want to mirror/steal a port to a different port on the remote
    /// machine. For example, your local process listens on port 9333 and the container listens
    /// on port 80. You'd use [[9333, 80]]
    pub port_mapping: Option<Vec<(u16, u16)>>,

    /// Consider removing when adding https://github.com/metalbear-co/mirrord/issues/702
    pub ignore_localhost: Option<bool>,

    /// Ports to ignore when mirroring/stealing traffic. Useful if you want
    /// specific ports to be used locally only.
    pub ignore_ports: Option<Vec<u16>>,
}

/// # incoming
///
/// Sets up how mirrord handles incoming network packets.
///
/// ## Minimal `incoming` config
///
/// ```json
/// {
///   "feature": {
///     "network": {
///       "incoming": "mirror",
///       "outgoing": true
///     }
///   }
/// }
/// ```
///
/// ## Advanced `incoming` config
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
///         }
///       }
///     }
///   }
/// }
/// ```
#[derive(Default, PartialEq, Eq, Clone, Debug)]
pub struct IncomingConfig {
    /// ## mode
    ///
    /// See incoming [`mode`](#mode incoming) for more details.
    pub mode: IncomingMode,

    /// ## http_header_filter
    ///
    /// See [`http_header_filter`](#http_header_filter) for more details.
    pub http_header_filter: http_filter::HttpHeaderFilterConfig,

    /// ## port_mapping
    pub port_mapping: BiMap<u16, u16>,

    /// ## ignore_localhost
    pub ignore_localhost: bool,

    /// ## ignore_ports
    pub ignore_ports: HashSet<u16>,
}

impl IncomingConfig {
    // rustdoc-stripper-ignore-next
    /// Helper function.
    ///
    /// Used by mirrord-layer to identify the incoming network configuration as steal or not.
    // rustdoc-stripper-ignore-next-stop
    pub fn is_steal(&self) -> bool {
        matches!(self.mode, IncomingMode::Steal)
    }
}

/// # mode incoming
///
/// Mode of operation for the incoming TCP traffic feature.
///
/// Can be set to either `"mirror"` (default) or `"steal"`.
#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum IncomingMode {
    /// ## mirror
    ///
    /// Sniffs on TCP port, and send a copy of the data to listeners.
    #[default]
    Mirror,

    /// ## steal
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
