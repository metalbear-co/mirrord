use std::{ops::Deref, str::FromStr};

use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    util::{MirrordToggleableConfig, VecOrSingle},
};

/// # filter
///
/// Filter configuration for the HTTP traffic stealer feature.
///
/// Allows the user to set a filter (regex) for the HTTP headers, so that the stealer traffic
/// feature only captures HTTP requests that match the specified filter, forwarding unmatched
/// requests to their original destinations.
///
/// Only does something when [`IncomingConfig`](super::IncomingConfig) is set as
/// [`IncomingMode::Steal`](super::IncomingMode::Steal), ignored otherwise.
///
/// ## Example `http_header_filter` config
///
/// ```json
/// {
///   "filter": "host: api\..+",
///   "ports": [80, 8080]
/// }
/// ```
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "HttpHeaderFilterFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct HttpHeaderFilterConfig {
    /// ### filter
    ///
    /// Used to match against the requests captured by the mirrord-agent pod.
    ///
    /// Supports regexes validated by the
    /// [`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate.
    ///
    /// #### Usage
    ///
    /// The HTTP traffic feature converts the HTTP headers to `HeaderKey: HeaderValue`,
    /// case-insensitive.
    #[config(env = "MIRRORD_HTTP_HEADER_FILTER")]
    pub filter: Option<String>,

    /// ### ports
    ///
    /// Activate the HTTP traffic filter only for these ports.
    #[config(env = "MIRRORD_HTTP_HEADER_FILTER_PORTS", default)]
    pub ports: PortList,
}

// rustdoc-stripper-ignore-next
/// Helper struct for setting up ports configuration (part of the HTTP traffic stealer feature).
///
/// Defaults to a list of ports `[80, 8080]`.
///
/// ## Internal
///
/// We use this to allow implementing a custom [`Default`] initialization, as the [`MirrordConfig`]
/// macro (currently) doesn't support more intricate expressions.
// rustdoc-stripper-ignore-next-stop
#[derive(PartialEq, Eq, Clone, Debug, JsonSchema, Serialize, Deserialize)]
pub struct PortList(VecOrSingle<u16>);

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

impl From<PortList> for Vec<u16> {
    fn from(value: PortList) -> Self {
        value.0.to_vec()
    }
}
