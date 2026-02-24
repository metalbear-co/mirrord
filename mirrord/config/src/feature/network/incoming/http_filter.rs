use std::{ops::Not, str::FromStr, sync::LazyLock};

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use mirrord_protocol::tcp::{
    Filter, HTTP_BODY_JSON_FILTER_VERSION, HTTP_COMPOSITE_FILTER_VERSION,
    HTTP_METHOD_FILTER_VERSION, HttpBodyFilter, HttpFilter, HttpMethodFilter, JsonPathQuery,
};
use schemars::JsonSchema;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    config::{ConfigContext, ConfigError, from_env::FromEnv, source::MirrordConfigSource},
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

    /// ##### feature.network.incoming.http_filter.method_filter {#feature-network-incoming-http-method-filter}
    ///
    ///
    /// Supports standard [HTTP methods](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Methods), and non-standard HTTP methods.
    ///
    /// Case-insensitive. If the request method matches the filter, the request is stolen.
    #[config(env = "MIRRORD_HTTP_METHOD_FILTER")]
    pub method_filter: Option<String>,

    /// ##### feature.network.incoming.http_filter.body_filter {#feature-network-incoming-http-body-filter}
    ///
    /// Matches the request based on the contents of its body.
    pub body_filter: Option<BodyFilter>,

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
    ///     { "method": "post" }
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
    ///     { "method": "post" }
    ///   ]
    /// }
    /// ```
    pub any_of: Option<Vec<InnerFilter>>,

    /// ##### feature.network.incoming.http_filter.ports {#feature-network-incoming-http_filter-ports}
    ///
    /// Activate the HTTP traffic filter only for these ports. When
    /// absent, filtering will be done for all ports.
    #[config(env = "MIRRORD_HTTP_FILTER_PORTS")]
    pub ports: Option<VecOrSingle<u16>>,
}

impl HttpFilterConfig {
    pub fn is_filter_set(&self) -> bool {
        self.header_filter.is_some()
            || self.path_filter.is_some()
            || self.method_filter.is_some()
            || self.all_of.is_some()
            || self.any_of.is_some()
            || self.body_filter.is_some()
    }

    pub fn ensure_usable_with(
        &self,
        agent_protocol_version: Option<Version>,
    ) -> Result<(), ConfigError> {
        #![allow(clippy::type_complexity)]
        static REQUIREMENTS: [(fn(&HttpFilterConfig) -> bool, &LazyLock<VersionReq>, &str); 3] = [
            (
                HttpFilterConfig::is_composite,
                &HTTP_COMPOSITE_FILTER_VERSION,
                "'any_of' or 'all_of' HTTP filter types",
            ),
            (
                HttpFilterConfig::has_method_filter,
                &HTTP_METHOD_FILTER_VERSION,
                "'method' http filter type",
            ),
            (
                HttpFilterConfig::has_json_body_filter,
                &HTTP_BODY_JSON_FILTER_VERSION,
                "JSON body filters",
            ),
        ];

        for (validator, version, what) in REQUIREMENTS {
            if validator(self)
                && agent_protocol_version
                    .as_ref()
                    .map(|v| version.matches(v))
                    .unwrap_or(false)
                    .not()
            {
                Err(ConfigError::Conflict(format!(
                    "Cannot use {what}, protocol version used by mirrord-agent must match {}. \
                    Consider using a newer version of mirrord-agent",
                    **version
                )))?
            }
        }

        Ok(())
    }

    fn is_composite(&self) -> bool {
        self.all_of.is_some() || self.any_of.is_some()
    }

    fn has_method_filter(&self) -> bool {
        self.method_filter.is_some()
            || self.all_of.as_ref().is_some_and(|composite| {
                composite
                    .iter()
                    .any(|f| matches!(f, InnerFilter::Method { .. }))
            })
            || self.any_of.as_ref().is_some_and(|composite| {
                composite
                    .iter()
                    .any(|f| matches!(f, InnerFilter::Method { .. }))
            })
    }

    fn has_json_body_filter(&self) -> bool {
        matches!(self.body_filter, Some(BodyFilter::Json { .. }))
            || self.all_of.as_ref().is_some_and(|composite| {
                composite
                    .iter()
                    .any(|f| matches!(f, InnerFilter::Body(BodyFilter::Json { .. })))
            })
            || self.any_of.as_ref().is_some_and(|composite| {
                composite
                    .iter()
                    .any(|f| matches!(f, InnerFilter::Body(BodyFilter::Json { .. })))
            })
    }

    /// Returns the number of ports that get filtered.
    pub fn count_filtered_ports(&self) -> u16 {
        if self.is_filter_set().not() {
            0
        } else {
            match &self.ports {
                // "SAFETY": can't have more than u16::MAX ports
                Some(list) => list.len() as u16,
                None => u16::MAX,
            }
        }
    }

    /// Converts this config into the protocol-level [`HttpFilter`].
    ///
    /// Returns an error if a filter expression is invalid. Panics if no filter is set
    /// (call [`is_filter_set`](Self::is_filter_set) first).
    pub fn as_protocol_http_filter(&self) -> Result<HttpFilter, HttpFilterParseError> {
        match self {
            HttpFilterConfig {
                path_filter: Some(path),
                header_filter: None,
                method_filter: None,
                body_filter: None,
                all_of: None,
                any_of: None,
                ports: _,
            } => Ok(HttpFilter::Path(Filter::new(path.into())?)),

            HttpFilterConfig {
                path_filter: None,
                header_filter: Some(header),
                method_filter: None,
                body_filter: None,
                all_of: None,
                any_of: None,
                ports: _,
            } => Ok(HttpFilter::Header(Filter::new(header.into())?)),

            HttpFilterConfig {
                path_filter: None,
                header_filter: None,
                method_filter: Some(method),
                body_filter: None,
                all_of: None,
                any_of: None,
                ports: _,
            } => Ok(HttpFilter::Method(HttpMethodFilter::from_str(method)?)),

            HttpFilterConfig {
                path_filter: None,
                header_filter: None,
                method_filter: None,
                body_filter: Some(filter),
                all_of: None,
                any_of: None,
                ports: _,
            } => Ok(HttpFilter::Body(filter.as_protocol_http_body_filter()?)),

            HttpFilterConfig {
                path_filter: None,
                header_filter: None,
                method_filter: None,
                body_filter: None,
                all_of: Some(filters),
                any_of: None,
                ports: _,
            } => Self::make_composite_filter(true, filters),

            HttpFilterConfig {
                path_filter: None,
                header_filter: None,
                method_filter: None,
                body_filter: None,
                all_of: None,
                any_of: Some(filters),
                ports: _,
            } => Self::make_composite_filter(false, filters),

            _ => panic!("No HTTP filters specified, this should have been caught earlier"),
        }
    }

    fn make_composite_filter(
        all: bool,
        filters: &[InnerFilter],
    ) -> Result<HttpFilter, HttpFilterParseError> {
        let filters = filters
            .iter()
            .map(|filter| match filter {
                InnerFilter::Path { path } => Ok(HttpFilter::Path(Filter::new(path.clone())?)),
                InnerFilter::Header { header } => {
                    Ok(HttpFilter::Header(Filter::new(header.clone())?))
                }
                InnerFilter::Method { method } => {
                    Ok(HttpFilter::Method(HttpMethodFilter::from_str(method)?))
                }
                InnerFilter::Body(body_filter) => Ok(HttpFilter::Body(
                    body_filter.as_protocol_http_body_filter()?,
                )),
            })
            .collect::<Result<Vec<_>, HttpFilterParseError>>()?;

        Ok(HttpFilter::Composite { all, filters })
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
    Header {
        header: String,
    },

    /// ##### feature.network.incoming.inner_filter.path_filter {#feature-network-incoming-inner-path-filter}
    ///
    ///
    /// Supports regexes validated by the
    /// [`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate.
    ///
    /// Case-insensitive. Tries to find match in the path (without query) and path+query.
    /// If any of the two matches, the request is stolen.
    Path {
        path: String,
    },

    Method {
        method: String,
    },

    /// ##### feature.network.incoming.inner_filter.body_filter {#feature-network-incoming-inner-body-filter}
    ///
    /// Matches the request based on the contents of its body. Currently only JSON body filtering is
    /// supported.
    Body(BodyFilter),
}

/// Currently only JSON body filtering is supported.
#[derive(PartialEq, Eq, Clone, Debug, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "body", rename_all = "lowercase")]
pub enum BodyFilter {
    /// ##### feature.network.incoming.inner_filter.body_filter.json {#feature-network-incoming-inner-body-filter-json}
    ///
    /// Tries to parse the body as a JSON object and find (a) matching subobjects(s).
    ///
    /// `query` should be a valid JSONPath (RFC 9535) query string.
    //
    /// `matches` should be a regex. Supports regexes validated by the
    /// [`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate
    ///
    /// Example:
    /// ```json
    /// "http_filter": {
    ///   "body_filter": {
    ///     "body": "json",
    ///     "query": "$.library.books[*]",
    ///     "matches": "^\\d{3,5}$"
    ///   }
    /// }
    /// ```
    /// will match
    /// ```json
    /// {
    ///   "library": {
    ///     "books": [
    ///       34555,
    ///       1233,
    ///       234
    ///       23432
    ///     ]
    ///   }
    /// }
    /// ```
    ///
    /// The filter will match if there is at least one query result.
    ///
    /// Non-string matches are stringified before being compared to
    /// the regex. To filter query results by type, the `typeof`
    /// [function extension](https://www.rfc-editor.org/rfc/rfc9535.html#name-function-extensions)
    /// is provided. It takes in a single `NodesType` parameter and
    /// returns `"null" | "bool" | "number" | "string" | "array" | "object"`,
    /// depending on the type of the argument. If not all nodes in the
    /// argument have the same type, it returns `nothing`.
    ///
    /// Example:
    ///
    /// ```json
    /// "body_filter": {
    ///   "body": "json",
    ///   "query": "$.books[?(typeof(@) == 'number')]",
    ///   "matches": "4$"
    /// }
    /// ```
    /// will match
    ///
    /// ```json
    /// {
    ///   "books": [
    ///     1111,
    ///     2222,
    ///     4444
    ///   ]
    /// }
    /// ```
    ///
    /// but not
    ///
    /// ```json
    /// {
    ///   "books": [
    ///     "1111",
    ///     "2222",
    ///     "4444"
    ///   ]
    /// }
    /// ```
    ///
    ///
    ///
    /// To use with with `all_of` or `any_of`, use the following syntax:
    /// ```json
    /// "http_filter": {
    ///   "all_of": [
    ///     {
    ///       "path": "/buildings"
    ///     },
    ///     {
    ///       "body": "json",
    ///       "query": "$.library.books[*]",
    ///       "matches": "^\\d{3,5}$"
    ///     }
    ///   ]
    /// }
    /// ```
    Json { query: String, matches: String },
}

impl BodyFilter {
    /// Converts this config into the protocol-level [`HttpBodyFilter`].
    pub fn as_protocol_http_body_filter(&self) -> Result<HttpBodyFilter, Box<fancy_regex::Error>> {
        match self {
            BodyFilter::Json { query, matches } => Ok(HttpBodyFilter::Json {
                query: JsonPathQuery::new_unchecked(query.clone()),
                matches: Filter::new(matches.clone())?,
            }),
        }
    }
}

impl MirrordToggleableConfig for HttpFilterFileConfig {
    fn disabled_config(context: &mut ConfigContext) -> Result<Self::Generated, ConfigError> {
        let header_filter = FromEnv::new("MIRRORD_HTTP_HEADER_FILTER")
            .source_value(context)
            .transpose()?;

        let path_filter = FromEnv::new("MIRRORD_HTTP_PATH_FILTER")
            .source_value(context)
            .transpose()?;

        let method_filter = FromEnv::new("MIRRORD_HTTP_METHOD_FILTER")
            .source_value(context)
            .transpose()?;

        let all_of = None;
        let any_of = None;

        let body_filter = None;

        let ports = FromEnv::new("MIRRORD_HTTP_FILTER_PORTS")
            .source_value(context)
            .transpose()?;

        Ok(Self::Generated {
            header_filter,
            path_filter,
            method_filter,
            body_filter,
            all_of,
            any_of,
            ports,
        })
    }
}

impl CollectAnalytics for &HttpFilterConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("header_filter", self.header_filter.is_some());
        analytics.add("path_filter", self.path_filter.is_some());
        analytics.add("ports", self.count_filtered_ports());
    }
}

/// Error returned when converting an [`HttpFilterConfig`] into an [`HttpFilter`].
#[derive(Error, Debug)]
pub enum HttpFilterParseError {
    #[error(transparent)]
    Regex(#[from] Box<fancy_regex::Error>),

    #[error(transparent)]
    Method(#[from] strum::ParseError),
}
