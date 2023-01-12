use std::str::FromStr;

use schemars::JsonSchema;
use serde::Deserialize;
use thiserror::Error;

use crate::{
    config::{
        from_env::FromEnv, source::MirrordConfigSource, ConfigError, FromMirrordConfig,
        MirrordConfig, Result,
    },
    util::{MirrordToggleableConfig, ToggleableConfig},
};

pub mod http_filter;

use http_filter::*;

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
/// see [`StealModeConfig`]);
///
/// ## Examples
///
/// - Mirror any incoming traffic:
///
/// ```toml
/// # mirrord-config.toml
///
/// [feature.network]
/// incoming = "mirror"    # for illustration purporses, it's the default
/// ```
///
/// - Steal incoming HTTP traffic, if the HTTP header matches "Id: token.*" (supports regex):
///
/// ```yaml
/// # mirrord-config.yaml
///
/// [feature.network.incoming]
/// mode = "steal"
///
/// [feature.network.incoming.http_header_filter]
/// filter = "Id: token.*"
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
}

#[derive(Default, PartialEq, Eq, Clone, Debug)]
pub struct IncomingConfig {
    pub mode: IncomingMode,

    pub http_header_filter: http_filter::HttpHeaderFilterConfig,
}

impl IncomingConfig {
    /// Helper function.
    ///
    /// Used by mirrord-layer to identify the incoming network configuration as steal or not.
    pub fn is_steal(&self) -> bool {
        matches!(self.mode, IncomingMode::Steal)
    }
}

/// Mode of operation for the incoming TCP traffic feature.
///
/// Defaults to [`IncomingMode::Mirror`].
#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum IncomingMode {
    /// Sniffs on TCP port, and send a copy of the data to listeners.
    #[default]
    Mirror,

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
