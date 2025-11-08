use std::{fmt::Debug, io::Read};

use fancy_regex::Regex;
use hyper::http::request::Parts;
use mirrord_protocol::tcp::HttpMethodFilter;
use serde_json::Value;
use serde_json_path::{
    JsonPath,
    functions::{NodesType, ValueType},
};
use tracing::Level;

/// Currently supported filtering criterias.
#[derive(Debug, Clone)]
pub enum HttpFilter {
    /// Header based filter.
    ///
    /// This [`Regex`] should be used against each header after transforming it to `k: v` format.
    Header(Regex),
    /// Path based filter.
    Path(Regex),
    /// Filter composed of multiple filters.
    Composite {
        /// If true, all filters must match, otherwise any filter can match.
        all: bool,
        /// Filters to use.
        filters: Vec<HttpFilter>,
    },
    Method(HttpMethodFilter),

    /// Filter based on request body
    Body(HttpBodyFilter),
}

#[derive(thiserror::Error, Debug)]
pub enum FilterCreationError {
    #[error("error compiling regex: {0}")]
    Regex(#[from] fancy_regex::Error),

    #[error("error compiling jsonpath query: {0}")]
    JsonPath(#[from] serde_json_path::ParseError),
}

impl TryFrom<&mirrord_protocol::tcp::HttpFilter> for HttpFilter {
    type Error = FilterCreationError;

    fn try_from(filter: &mirrord_protocol::tcp::HttpFilter) -> Result<Self, Self::Error> {
        match filter {
            mirrord_protocol::tcp::HttpFilter::Header(header) => {
                Ok(Self::Header(Regex::new(&format!("(?i){header}"))?))
            }
            mirrord_protocol::tcp::HttpFilter::Path(path) => {
                Ok(Self::Path(Regex::new(&format!("(?i){path}"))?))
            }
            mirrord_protocol::tcp::HttpFilter::Method(method) => Ok(Self::Method(method.clone())),
            mirrord_protocol::tcp::HttpFilter::Composite { all, filters } => {
                let all = *all;
                let filters = filters
                    .iter()
                    .map(HttpFilter::try_from)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Self::Composite { all, filters })
            }
            mirrord_protocol::tcp::HttpFilter::Body(http_body_filter) => {
                Ok(Self::Body(http_body_filter.try_into()?))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum HttpBodyFilter {
    Json { query: JsonPath, matches: Regex },
}

impl TryFrom<&mirrord_protocol::tcp::HttpBodyFilter> for HttpBodyFilter {
    type Error = FilterCreationError;

    fn try_from(value: &mirrord_protocol::tcp::HttpBodyFilter) -> Result<Self, Self::Error> {
        Ok(match value {
            mirrord_protocol::tcp::HttpBodyFilter::Json { query, matches } => Self::Json {
                query: JsonPath::parse(query)?,
                matches: Regex::new(matches)?,
            },
        })
    }
}

#[serde_json_path::function(name = "typeof")]
fn type_of(nodes: NodesType) -> ValueType {
    #[derive(PartialEq, strum_macros::Display)]
    #[strum(serialize_all = "lowercase")]
    enum Types {
        Null,
        Bool,
        Number,
        String,
        Array,
        Object,
    }

    impl From<Types> for ValueType<'static> {
        fn from(t: Types) -> Self {
            ValueType::Value(Value::String(t.to_string()))
        }
    }

    impl From<&Value> for Types {
        fn from(v: &Value) -> Self {
            match v {
                Value::Null => Self::Null,
                Value::Bool(_) => Self::Bool,
                Value::Number(_) => Self::Number,
                Value::String(_) => Self::String,
                Value::Array(_) => Self::Array,
                Value::Object(_) => Self::Object,
            }
        }
    }

    let mut iter = nodes.iter();

    tracing::error!(?nodes, "lol");

    let first_type = match iter.next() {
        Some(first) => Types::from(*first),
        None => return ValueType::Nothing,
    };

    tracing::error!(%first_type, "not undefined");

    for v in iter {
        tracing::error!(?v, "json match");
        if Types::from(*v) != first_type {
            return ValueType::Nothing;
        }
    }

    first_type.into()
}

impl HttpFilter {
    /// Checks whether the given request [`Parts`] match this filter.
    #[tracing::instrument(level = Level::DEBUG, skip_all, fields(has_body = body.is_some()), ret)]
    pub fn matches<T: Read + Copy>(&self, parts: &mut Parts, body: Option<T>) -> bool {
        match self {
            Self::Header(filter) => {
                let headers = parts.extensions.get_or_insert_with(|| {
                    let normalized = parts
                        .headers
                        .iter()
                        .filter_map(|(header_name, header_value)| {
                            header_value
                                .to_str()
                                .ok()
                                .map(|header_value| format!("{header_name}: {header_value}"))
                        })
                        .collect::<Vec<_>>();

                    NormalizedHeaders(normalized)
                });

                headers.has_match(filter)
            }

            Self::Path(filter) => parts.uri.path_and_query().is_some_and(|path_and_query| {
                // For backward compatability, we first match path then we match path and query
                // together and return true if any of them matches
                let path = path_and_query.path();
                let matched = filter
                    .is_match(path)
                    .inspect_err(|error| {
                        tracing::error!(path, ?error, "Error while matching path");
                    })
                    .unwrap_or(false);
                if matched {
                    return true;
                }

                let path = path_and_query.as_str();
                filter
                    .is_match(path)
                    .inspect_err(|error| {
                        tracing::error!(path, ?error, "Error while matching path+query");
                    })
                    .unwrap_or(false)
            }),

            Self::Method(filter) => parts.method.as_str().eq_ignore_ascii_case(filter.as_ref()),

            Self::Composite { all: true, filters } => {
                filters.iter().all(|f| f.matches(parts, body))
            }
            Self::Composite {
                all: false,
                filters,
            } => filters.iter().any(|f| f.matches(parts, body)),
            Self::Body(filter) => {
                let Some(body) = body else { return false };

                match filter {
                    HttpBodyFilter::Json { query, matches } => {
                        let json = match serde_json::from_reader::<_, Value>(body) {
                            Ok(json) => json,
                            Err(error) => {
                                tracing::trace!(?error, "json filter failed to parse body json");
                                return false;
                            }
                        };

                        let results = query.query(&json);

                        results.iter().any(|v| {
                            match v {
                                Value::String(s) => matches.is_match(s),
                                other => matches.is_match(&other.to_string()),
                            }
                            .is_ok_and(|t| t)
                        })
                    }
                }
            }
        }
    }

    pub fn needs_body(&self) -> bool {
        match self {
            HttpFilter::Composite { filters, .. } => filters.iter().any(|t| t.needs_body()),
            HttpFilter::Body(_) => true,
            _ => false,
        }
    }
}

/// [`HeaderMap`](hyper::http::header::HeaderMap) entries formatted like `k: v` (format expected by
/// [`HttpFilter::Header`]). Computed and cached in [`Parts::extensions`] the first time
/// [`HttpFilter::matches`] is called on [`Parts`].
#[derive(Clone, Debug)]
struct NormalizedHeaders(Vec<String>);

impl NormalizedHeaders {
    /// Checks whether any header in this set matches the given [`Regex`].
    fn has_match(&self, regex: &Regex) -> bool {
        self.0.iter().any(|header| {
            regex
                .is_match(header)
                .inspect_err(|error| {
                    tracing::error!(header, ?regex, ?error, "Error while matching header");
                })
                .unwrap_or_default()
        })
    }
}

#[cfg(test)]
mod test {
    use std::{ops::Not, str::FromStr};

    use hyper::Request;
    use mirrord_protocol::tcp::{self, Filter, HttpMethodFilter};

    use super::HttpFilter;

    #[test]
    fn matching_all_filter() {
        let tcp_filter = tcp::HttpFilter::Composite {
            all: true,
            filters: vec![
                tcp::HttpFilter::Header(Filter::new("brass-key: a-bazillion".to_string()).unwrap()),
                tcp::HttpFilter::Path(Filter::new("path/to/v1".to_string()).unwrap()),
                tcp::HttpFilter::Method(HttpMethodFilter::from_str("get").unwrap()),
            ],
        };

        // should match
        let mut input = Request::builder()
            .method("GET")
            .uri("https://www.balconia.gov/api/path/to/v1")
            .header("brass-key", "a-bazillion")
            .body(())
            .unwrap()
            .into_parts()
            .0;
        let filter: HttpFilter = TryFrom::try_from(&tcp_filter).unwrap();
        assert!(filter.matches::<&[u8]>(&mut input, None));

        // should fail
        let mut input = Request::builder()
            .method("POST")
            .uri("https://www.balconia.gov/api/path/to/v1")
            .header("brass-key", "nothin")
            .body(())
            .unwrap()
            .into_parts()
            .0;
        assert!(filter.matches::<&[u8]>(&mut input, None).not());
    }

    #[test]
    fn matching_any_filter() {
        let tcp_filter = tcp::HttpFilter::Composite {
            all: false,
            filters: vec![
                tcp::HttpFilter::Header(Filter::new("brass-key: a-bazillion".to_string()).unwrap()),
                tcp::HttpFilter::Header(Filter::new("dungeon-key: heavy".to_string()).unwrap()),
                tcp::HttpFilter::Path(Filter::new("path/to/v1".to_string()).unwrap()),
                tcp::HttpFilter::Path(Filter::new("path/for/v8".to_string()).unwrap()),
                tcp::HttpFilter::Method(HttpMethodFilter::from_str("get").unwrap()),
            ],
        };

        // should match
        let mut input = Request::builder()
            .method("GET")
            .uri("https://www.balconia.gov/api/path/to/v1")
            .header("brass-key", "nothin")
            .body(())
            .unwrap()
            .into_parts()
            .0;
        let filter: HttpFilter = TryFrom::try_from(&tcp_filter).unwrap();
        assert!(filter.matches::<&[u8]>(&mut input, None));

        // should fail
        let mut input = Request::builder()
            .method("POST")
            .uri("https://www.balconia.gov/api/path/to/v3")
            .header("brass-key", "nothin")
            .body(())
            .unwrap()
            .into_parts()
            .0;
        let filter: HttpFilter = TryFrom::try_from(&tcp_filter).unwrap();
        assert!(!filter.matches::<&[u8]>(&mut input, None));
    }
}
