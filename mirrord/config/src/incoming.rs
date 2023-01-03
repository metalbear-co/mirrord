use std::str::FromStr;

use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::Deserialize;
use thiserror::Error;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    util::MirrordToggleableConfig,
};

/// Controls the mode of operation for incoming traffic.
///
/// Defaults to [`IncomingMode::Mirror`].
///
/// See the incoming [reference](https://mirrord.dev/docs/reference/traffic/#incoming) for more
/// details.
///
/// Incoming traffic supports 2 modes of operation:
///
/// 1. Mirror: Sniffs the TCP data from a port, and forwards a copy to the interested listeners;
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
/// http_filter = "Id: token.*"
/// ```
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "IncomingFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct IncomingConfig {
    #[config(env = "MIRRORD_AGENT_TCP_STEAL_TRAFFIC", default = IncomingMode::Mirror)]
    pub mode: IncomingMode,

    #[config(env = "MIRRORD_HTTP_FILTER")]
    pub http_filter: Option<String>,
}

impl MirrordToggleableConfig for IncomingFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let filter = FromEnv::new("MIRRORD_HTTP_FILTER")
            .source_value()
            .transpose()?;

        let mode = FromEnv::new("MIRRORD_AGENT_TCP_STEAL_TRAFFIC")
            .source_value()
            .unwrap_or_else(|| Ok(Default::default()))?;

        Ok(IncomingConfig {
            mode,
            http_filter: filter,
        })
    }
}

impl IncomingConfig {
    /// Helper function.
    ///
    /// Used by mirrord-layer to identify the incoming network configuration as steal or not.
    pub fn is_steal(&self) -> bool {
        matches!(self.mode, IncomingMode::Steal)
    }
}

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
