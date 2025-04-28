use std::{collections::HashSet, ops::Deref, str::FromStr};

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigContext, ConfigError},
    util::{MirrordToggleableConfig, VecOrSingle},
};

/// Filter configuration for the HTTP traffic stealer feature.
///
/// Allows the user to set a filter (regex) for the HTTP headers, so that the stealer traffic
/// feature only captures HTTP requests that match the specified filter, forwarding unmatched
/// requests to their original destinations.
///
/// Only does something when [`feature.network.incoming.mode`](#feature-network-incoming-mode) is
/// set as `"steal"`, ignored otherwise.
///
/// For example, to filter based on header:
/// ```json
/// {
///   "header_filter": "host: api\\..+"
/// }
/// ```
/// Setting that filter will make mirrord only steal requests with the `host` header set to hosts
/// that start with "api", followed by a dot, and then at least one more character.
///
/// For example, to filter based on path:
/// ```json
/// {
///   "path_filter": "^/api/"
/// }
/// ```
/// Setting this filter will make mirrord only steal requests to URIs starting with "/api/".
///
///
/// This can be useful for filtering out Kubernetes liveness, readiness and startup probes.
/// For example, for avoiding stealing any probe sent by kubernetes, you can set this filter:
/// ```json
/// {
///   "header_filter": "^User-Agent: (?!kube-probe)"
/// }
/// ```
/// Setting this filter will make mirrord only steal requests that **do** have a user agent that
/// **does not** begin with "kube-probe".
///
/// Similarly, you can exclude certain paths using a negative look-ahead:
/// ```json
/// {
///   "path_filter": "^(?!/health/)"
/// }
/// ```
/// Setting this filter will make mirrord only steal requests to URIs that do not start with
/// "/health/".
///
/// With `all_of` and `any_of`, you can use multiple HTTP filters at the same time.
///
/// If you want to steal HTTP requests that match **every** pattern specified, use `all_of`.
/// For example, this filter steals only HTTP requests to endpoint `/api/my-endpoint` that contain
/// header `x-debug-session` with value `121212`.
/// ```json
/// {
///   "all_of": [
///     { "header": "^x-debug-session: 121212$" },
///     { "path": "^/api/my-endpoint$" }
///   ]
/// }
/// ```
///
/// If you want to steal HTTP requests that match **any** of the patterns specified, use `any_of`.
/// For example, this filter steals HTTP requests to endpoint `/api/my-endpoint`
/// **and** HTTP requests that contain header `x-debug-session` with value `121212`.
/// ```json
/// {
///  "any_of": [
///    { "path": "^/api/my-endpoint$"},
///    { "header": "^x-debug-session: 121212$" }
///  ]
/// }
/// ```
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
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
    #[config(env = "MIRRORD_HTTP_HEADER_FILTER")]
    pub header_filter: Option<String>,

    /// ##### feature.network.incoming.http_filter.path_filter {#feature-network-incoming-http-path-filter}
    ///
    ///
    /// Supports regexes validated by the
    /// [`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate.
    ///
    /// Case-insensitive. Tries to find match in the path (without query) and path+query.
    /// If any of the two matches, the request is stolen.
    #[config(env = "MIRRORD_HTTP_PATH_FILTER")]
    pub path_filter: Option<String>,

    /// ##### feature.network.incoming.http_filter.all_of {#feature-network-incoming-http_filter-all_of}
    ///
    /// An array of HTTP filters.
    ///
    /// Each inner filter specifies either header or path regex.
    /// Requests must match all of the filters to be stolen.
    ///
    /// Cannot be an empty list.
    ///
    /// Example:
    /// ```json
    /// {
    ///   "all_of": [
    ///     { "header": "x-user: my-user$" },
    ///     { "path": "^/api/v1/my-endpoint" }
    ///   ]
    /// }
    /// ```
    pub all_of: Option<Vec<InnerFilter>>,

    /// ##### feature.network.incoming.http_filter.any_of {#feature-network-incoming-http_filter-any_of}
    ///
    /// An array of HTTP filters.
    ///
    /// Each inner filter specifies either header or path regex.
    /// Requests must match at least one of the filters to be stolen.
    ///
    /// Cannot be an empty list.
    ///
    /// Example:
    /// ```json
    /// {
    ///   "any_of": [
    ///     { "header": "^x-user: my-user$" },
    ///     { "path": "^/api/v1/my-endpoint" }
    ///   ]
    /// }
    /// ```
    pub any_of: Option<Vec<InnerFilter>>,

    /// ##### feature.network.incoming.http_filter.ports {#feature-network-incoming-http_filter-ports}
    ///
    /// Activate the HTTP traffic filter only for these ports.
    ///
    /// Other ports will *not* be stolen, unless listed in
    /// [`feature.network.incoming.ports`](#feature-network-incoming-ports).
    ///
    /// We check the pod's health probe ports and automatically add them here, as they're
    /// usually the same ports your app might be listening on. If your app ports and the
    /// health probe ports don't match, then setting this option will override this behavior.
    ///
    /// Set to [80, 8080] by default.
    #[config(env = "MIRRORD_HTTP_FILTER_PORTS")]
    pub ports: Option<PortList>,
}

impl HttpFilterConfig {
    pub fn is_filter_set(&self) -> bool {
        self.header_filter.is_some()
            || self.path_filter.is_some()
            || self.all_of.is_some()
            || self.any_of.is_some()
    }

    pub fn is_composite(&self) -> bool {
        self.all_of.is_some() || self.any_of.is_some()
    }

    pub fn get_filtered_ports(&self) -> Option<&[u16]> {
        if let Some(ports) = self.ports.as_ref()
            && self.is_filter_set()
        {
            Some(&*ports.0)
        } else {
            None
        }
    }
}

#[derive(PartialEq, Eq, Clone, Debug, JsonSchema, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InnerFilter {
    /// ##### feature.network.incoming.inner_filter.header_filter {#feature-network-incoming-inner-header-filter}
    ///
    ///
    /// Supports regexes validated by the
    /// [`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate.
    ///
    /// The HTTP traffic feature converts the HTTP headers to `HeaderKey: HeaderValue`,
    /// case-insensitive.
    Header { header: String },

    /// ##### feature.network.incoming.inner_filter.path_filter {#feature-network-incoming-inner-path-filter}
    ///
    ///
    /// Supports regexes validated by the
    /// [`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate.
    ///
    /// Case-insensitive. Tries to find match in the path (without query) and path+query.
    /// If any of the two matches, the request is stolen.
    Path { path: String },
}

/// <!--${internal}-->
/// Helper struct for setting up ports configuration (part of the HTTP traffic stealer feature).
///
/// Defaults to a list of ports `[80, 8080]`.
///
/// We use this to allow implementing a custom [`Default`] initialization, as the [`MirrordConfig`]
/// macro (currently) doesn't support more intricate expressions.
#[derive(PartialEq, Eq, Clone, Debug, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortList(VecOrSingle<u16>);

impl MirrordToggleableConfig for HttpFilterFileConfig {
    fn disabled_config(context: &mut ConfigContext) -> Result<Self::Generated, ConfigError> {
        let header_filter = FromEnv::new("MIRRORD_HTTP_HEADER_FILTER")
            .source_value(context)
            .transpose()?;

        let path_filter = FromEnv::new("MIRRORD_HTTP_PATH_FILTER")
            .source_value(context)
            .transpose()?;

        let all_of = None;
        let any_of = None;

        let ports = FromEnv::new("MIRRORD_HTTP_FILTER_PORTS")
            .source_value(context)
            .transpose()?;

        Ok(Self::Generated {
            header_filter,
            path_filter,
            all_of,
            any_of,
            ports,
        })
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

impl From<Vec<u16>> for PortList {
    fn from(value: Vec<u16>) -> Self {
        PortList(VecOrSingle::Multiple(value))
    }
}

impl From<PortList> for HashSet<u16> {
    fn from(value: PortList) -> Self {
        value.0.into()
    }
}

impl core::fmt::Display for PortList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[")?;
        for port in self.iter() {
            write!(f, "{port} ")?;
        }
        write!(f, "]")?;
        Ok(())
    }
}

impl CollectAnalytics for &HttpFilterConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("header_filter", self.header_filter.is_some());
        analytics.add("path_filter", self.path_filter.is_some());
        analytics.add(
            "ports",
            self.get_filtered_ports()
                .map(|p| p.len())
                .unwrap_or_default(),
        );
    }
}
