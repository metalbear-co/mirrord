use std::{collections::HashSet, ops::Deref, str::FromStr};

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    util::{MirrordToggleableConfig, VecOrSingle},
};

// TODO(alex) [high] 2023-05-18: Wrong example (incomplete) here and in the oficial docs.
/// Filter configuration for the HTTP traffic stealer feature.
///
/// DEPRECATED - USE http_filter instead, unless using old operator/agent version (pre 3.46.0)
/// Allows the user to set a filter (regex) for the HTTP headers, so that the stealer traffic
/// feature only captures HTTP requests that match the specified filter, forwarding unmatched
/// requests to their original destinations.
///
/// Only does something when [`feature.network.incoming.mode`](#feature-network-incoming-mode) is
/// set as `"steal"`, ignored otherwise.
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
    /// ##### feature.network.incoming.http_header_filter.filter {#feature-network-incoming-http_header_filter-filter}
    ///
    ///
    /// Supports regexes validated by the
    /// [`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate.
    ///
    /// The HTTP traffic feature converts the HTTP headers to `HeaderKey: HeaderValue`,
    /// case-insensitive.
    #[config(
        env = "MIRRORD_HTTP_HEADER_FILTER",
        deprecated = "use http_filter instead"
    )]
    pub filter: Option<String>,

    /// ##### feature.network.incoming.http_header_filter.ports {#feature-network-incoming-http_header_filter-ports}
    ///
    /// Activate the HTTP traffic filter only for these ports.
    ///
    /// Other ports will still be stolen (when `"steal`" is being used), they're just not checked
    /// for HTTP filtering.
    #[config(env = "MIRRORD_HTTP_HEADER_FILTER_PORTS", default)]
    pub ports: PortList,
}

/// Filter configuration for the HTTP traffic stealer feature.
///
/// Allows the user to set a filter (regex) for the HTTP headers, so that the stealer traffic
/// feature only captures HTTP requests that match the specified filter, forwarding unmatched
/// requests to their original destinations.
///
/// Only does something when [`feature.network.incoming.mode`](#feature-network-incoming-mode) is
/// set as `"steal"`, ignored otherwise.
///
/// for example, to filter based on header:
/// ```json
/// {
///   "header_filter": "host: api\..+",
/// }
/// ```
///
/// for example, to filter based on path
/// ```json
/// {
///   "path_filter": "host: api\..+",
/// }
/// ```
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "HttpFilterFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct HttpFilterConfig {
    /// ##### feature.network.incoming.http_filter.header_filter {#feature-network-incoming-http-header-filter}
    ///
    ///
    /// Supports regexes validated by the
    /// [`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate.
    ///
    /// The HTTP traffic feature converts the HTTP headers to `HeaderKey: HeaderValue`,
    /// case-insensitive.
    pub header_filter: Option<String>,

    /// ##### feature.network.incoming.http_filter.path_filter {#feature-network-incoming-http-path-filter}
    ///
    ///
    /// Supports regexes validated by the
    /// [`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate.
    ///
    /// Case insensitive.
    pub path_filter: Option<String>,

    /// ##### feature.network.incoming.http_header_filter.ports {#feature-network-incoming-http_header_filter-ports}
    ///
    /// Activate the HTTP traffic filter only for these ports.
    ///
    /// Other ports will still be stolen (when `"steal`" is being used), they're just not checked
    /// for HTTP filtering.
    #[config(default)]
    pub ports: PortList,
}

/// <!--${internal}-->
/// Helper struct for setting up ports configuration (part of the HTTP traffic stealer feature).
///
/// Defaults to a list of ports `[80, 8080]`.
///
/// We use this to allow implementing a custom [`Default`] initialization, as the [`MirrordConfig`]
/// macro (currently) doesn't support more intricate expressions.
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

impl MirrordToggleableConfig for HttpFilterFileConfig {
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(HttpFilterConfig::default())
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

impl From<PortList> for HashSet<u16> {
    fn from(value: PortList) -> Self {
        value.0.into()
    }
}

impl CollectAnalytics for HttpFilterConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("header_filter", self.header_filter.is_some());
        analytics.add("path_filter", self.path_filter.is_some());
        analytics.add("ports", self.ports.len());
    }
}
