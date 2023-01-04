use std::{ops::Deref, str::FromStr};

use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    util::{MirrordToggleableConfig, VecOrSingle},
};

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
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "IncomingFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct IncomingConfig {
    /// Allows selecting between mirrorring or stealing traffic.
    ///
    /// See [`IncomingMode`] for details.
    #[config(env = "MIRRORD_AGENT_TCP_STEAL_TRAFFIC", default = IncomingMode::Mirror)]
    pub mode: IncomingMode,

    /// Sets up the HTTP traffic filter (currently, only for [`IncomingMode::Steal`]).
    ///
    /// See [`HttpHeaderFilterConfig`] for details.
    #[config(toggleable, nested)]
    pub http_header_filter: HttpHeaderFilterConfig,
}

/// Helper struct for setting up ports configuration (part of the HTTP traffic stealer feature).
///
/// Defaults to a list of ports `[80, 8080]`.
///
/// ## Internal
///
/// We use this to allow implementing a custom [`Default`] initialization, as the [`MirrordConfig`]
/// macro (currently) doesn't support more intricate expressions.
#[derive(PartialEq, Eq, Clone, Debug, JsonSchema, Serialize, Deserialize)]
pub struct PortList(VecOrSingle<u16>);

impl Default for PortList {
    fn default() -> Self {
        Self(VecOrSingle::Multiple(vec![80, 8080]))
    }
}

impl Deref for PortList {
    type Target = VecOrSingle<u16>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FromStr for PortList {
    type Err = <VecOrSingle<u16> as FromStr>::Err;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(PortList)
    }
}

impl Into<Vec<u16>> for PortList {
    fn into(self) -> Vec<u16> {
        self.0.to_vec()
    }
}

/// Filter configuration for the HTTP traffic stealer feature.
///
/// Allows the user to set a filter (regex) for the HTTP headers, so that the stealer traffic
/// feature only captures HTTP requests that match the specified filter, forwarding unmatched
/// requests to their original destinations.
///
/// Only does something when [`IncomingConfig`] is set as [`IncomingMode::Steal`], ignored
/// otherwise.
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "HttpHeaderFilterFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct HttpHeaderFilterConfig {
    /// Used to match against the requests captured by the mirrord-agent pod.
    ///
    /// Supports regexes validated by the
    /// [`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate.
    ///
    /// ## Usage
    ///
    /// The HTTP traffic feature converts the HTTP headers to `HeaderKey: HeaderValue`,
    /// case-insensitive.
    #[config(env = "MIRRORD_HTTP_HEADER_FILTER")]
    pub filter: Option<String>,

    /// Activate the HTTP traffic filter only for these ports.
    #[config(env = "MIRRORD_HTTP_HEADER_FILTER_PORTS", default)]
    pub ports: PortList,
}

impl MirrordToggleableConfig for HttpHeaderFilterFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        let filter = FromEnv::new("MIRRORD_HTTP_HEADER_FILTER")
            .source_value()
            .transpose()?;

        let ports = FromEnv::new("MIRRORD_HTTP_HEADER_FILTER_PORTS")
            .source_value()
            .transpose()?
            .unwrap_or_default();

        Ok(Self::Generated { filter, ports })
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
