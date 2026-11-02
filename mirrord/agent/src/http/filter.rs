use std::{
    fmt::{self, Debug},
    io::Read,
    ops::Not,
    sync::LazyLock,
    time::Duration,
};

use fancy_regex::Regex;
use http::HeaderMap;
use hyper::http::request::Parts;
use jaq_core::{
    Ctx, RcIter,
    load::{Arena, File, Loader},
};
use jaq_json::Val;
use mirrord_agent_env::envs::JAQ_TIME_LIMIT;
use mirrord_protocol::tcp::{HttpMethodFilter, JqQuery};
use serde_json::Value;
use serde_json_path::JsonPath;
use tokio_retry::strategy::ExponentialBackoff;
use tracing::{Instrument, Level};

/// What a filter can see of a request body when asked whether the request matches.
#[derive(Clone, Copy)]
pub enum RequestBody<T> {
    /// Still arriving. A filter that needs it answers [`MatchResult::NeedsBody`], so the caller
    /// buffers the body and asks again.
    Pending,
    /// Was never buffered (over the size limit, or too slow to arrive) and never will be. A body
    /// filter cannot match it: a user stealing by body does not get requests nobody could check.
    /// A `Not` over it therefore matches, which is the safe side for a preview pod that steals
    /// everything but preview traffic: a request whose body nobody could check goes back to the
    /// stable pods instead of being served by preview code.
    Unavailable,
    /// Fully buffered.
    Complete(T),
}

impl<T> Debug for RequestBody<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pending => "Pending",
            Self::Unavailable => "Unavailable",
            Self::Complete(..) => "Complete",
        })
    }
}

/// Answer of [`HttpFilter::matches`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchResult {
    Match,
    NoMatch,
    /// The headers alone cannot decide and the body is still [`RequestBody::Pending`]. Buffer it
    /// and ask again.
    NeedsBody,
}

impl From<bool> for MatchResult {
    fn from(matches: bool) -> Self {
        if matches { Self::Match } else { Self::NoMatch }
    }
}

impl Not for MatchResult {
    type Output = Self;

    fn not(self) -> Self {
        match self {
            Self::Match => Self::NoMatch,
            Self::NoMatch => Self::Match,
            Self::NeedsBody => Self::NeedsBody,
        }
    }
}

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

    /// Header based on header using jq
    HeaderJq(JqQuery),

    /// Matches whatever the inner filter does not.
    Not(Box<HttpFilter>),
}

#[derive(thiserror::Error, Debug)]
pub enum FilterCreationError {
    #[error("error compiling regex: {0}")]
    Regex(#[from] fancy_regex::Error),

    #[error("error compiling jsonpath query: {0}")]
    JsonPath(#[from] serde_json_path::ParseError),

    #[error("error compiling jq expression: {0}")]
    Jq(String),
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
            mirrord_protocol::tcp::HttpFilter::HeaderJq(header) => {
                // Recompile to validate again
                JqQuery::new(header)
                    .map(HttpFilter::HeaderJq)
                    .map_err(FilterCreationError::Jq)
            }
            mirrord_protocol::tcp::HttpFilter::Not(filter) => {
                Ok(Self::Not(Box::new(HttpFilter::try_from(filter.as_ref())?)))
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

impl HttpFilter {
    /// Checks whether the given request [`Parts`] match this filter.
    ///
    /// Headers decide as much as they can on their own: a composite short-circuits on a definite
    /// answer and only reports [`MatchResult::NeedsBody`] when some body filter is the one left
    /// undecided, so a request is buffered only when its body really is the deciding factor.
    #[tracing::instrument(level = Level::DEBUG, skip_all, fields(?body), ret)]
    pub async fn matches<T: Read + Copy>(
        &self,
        parts: &mut Parts,
        body: RequestBody<T>,
    ) -> MatchResult {
        match self {
            Self::Header(filter) => {
                let headers = parts
                    .extensions
                    .get_or_insert_with(|| NormalizedHeaders::from_headers(&parts.headers));

                headers.has_match(filter).into()
            }

            Self::Path(filter) => parts
                .uri
                .path_and_query()
                .is_some_and(|path_and_query| {
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
                })
                .into(),

            Self::Method(filter) => parts
                .method
                .as_str()
                .eq_ignore_ascii_case(filter.as_ref())
                .into(),

            // Flips a definite answer and leaves "needs the body" to the caller: negating "cannot
            // look yet" would steal or mirror the request before its body is read.
            Self::Not(filter) => Box::pin(filter.matches(parts, body)).await.not(),

            Self::Composite { all: true, filters } => {
                // Since we require `body` to be Clone + Copy, each
                // iteration creates a new version that reads from the
                // beginning. Need to make sure that we don't read
                // anything from `body` before passing (copies of) it
                // to other fns.
                let mut result = MatchResult::Match;
                for filter in filters {
                    match Box::pin(filter.matches(parts, body)).await {
                        MatchResult::NoMatch => return MatchResult::NoMatch,
                        MatchResult::NeedsBody => result = MatchResult::NeedsBody,
                        MatchResult::Match => {}
                    }
                }
                result
            }
            Self::Composite {
                all: false,
                filters,
            } => {
                // Same as above
                let mut result = MatchResult::NoMatch;
                for filter in filters {
                    match Box::pin(filter.matches(parts, body)).await {
                        MatchResult::Match => return MatchResult::Match,
                        MatchResult::NeedsBody => result = MatchResult::NeedsBody,
                        MatchResult::NoMatch => {}
                    }
                }
                result
            }
            Self::Body(filter) => {
                let body = match body {
                    RequestBody::Pending => return MatchResult::NeedsBody,
                    RequestBody::Unavailable => return MatchResult::NoMatch,
                    RequestBody::Complete(body) => body,
                };

                match filter {
                    HttpBodyFilter::Json { query, matches } => {
                        let json = match serde_json::from_reader::<_, Value>(body) {
                            Ok(json) => json,
                            Err(error) => {
                                tracing::debug!(?error, "json filter failed to parse body json");
                                return MatchResult::NoMatch;
                            }
                        };

                        let results = query.query(&json);

                        results
                            .iter()
                            .any(|v| {
                                match v {
                                    Value::String(s) => matches.is_match(s),
                                    other => matches.is_match(&other.to_string()),
                                }
                                .is_ok_and(|t| t)
                            })
                            .into()
                    }
                }
            }
            Self::HeaderJq(filter) => {
                let headers = parts
                    .extensions
                    .get_or_insert_with(|| NormalizedHeaders::from_headers(&parts.headers));

                for header in headers.0.iter() {
                    match eval_jaq(filter.clone(), header.clone()).await {
                        Ok(true) => return MatchResult::Match,
                        Ok(false) => (),
                        Err(err) => {
                            tracing::error!(?err, ?header, ?filter, "failed to run jaq query");
                        }
                    }
                }

                MatchResult::NoMatch
            }
        }
    }
}

async fn eval_jaq(query: JqQuery, payload: String) -> Result<bool, String> {
    static TIME_LIMIT: LazyLock<Duration> = LazyLock::new(|| {
        Duration::from_secs(JAQ_TIME_LIMIT.try_from_env().ok().flatten().unwrap_or(500))
    });

    let span = tracing::warn_span!("jaq eval", ?query);

    let mut handle = tokio::task::spawn_blocking(move || {
        let program = File {
            code: query.as_str(),
            path: (),
        };
        let loader = Loader::new(jaq_std::defs().chain(jaq_json::defs()));
        let arena = Arena::default();
        let modules = loader
            .load(&arena, program)
            .map_err(|errors| format!("failed to parse the filter: {errors:?}"))?;

        let filter =
            jaq_core::Compiler::default().with_funs(jaq_std::funs().chain(jaq_json::funs()));

        let filter = filter
            .compile(modules)
            .map_err(|errors| format!("failed to compile the filter: {errors:?}"))?;

        let inputs = RcIter::new(core::iter::empty());
        let mut out = filter.run((Ctx::new([], &inputs), Val::from(payload)));

        let found_match = out
            .find_map(|item| {
                if let Ok(Val::Bool(value)) = &item {
                    Some(*value)
                } else {
                    None
                }
            })
            .unwrap_or(false);
        Ok(found_match)
    });

    tokio::select! {
        result = &mut handle => {
            match result {
                Ok(r) => r,
                Err(join) => {
                    tracing::error!(?join, "panic in jaq evaluation task");
                    Ok(false)
                }
            }
        }
        _ = tokio::time::sleep(*TIME_LIMIT) => {
            tracing::warn!("jq expr evaluation took longer than max allowed time");

            tokio::spawn(async move {
                let backoff = ExponentialBackoff::from_millis(5)
                    .factor(200)
                    .max_delay(Duration::from_secs(25));

                for delay in backoff {
                    tokio::time::sleep(delay).await;
                    tracing::warn!("jq evaluation has not completed");
                }
            }.instrument(span));

            Ok(false)
        }
    }
}

/// [`HeaderMap`] entries formatted like `k: v` (format expected by
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

    fn from_headers(headers: &HeaderMap) -> Self {
        Self(
            headers
                .iter()
                .filter_map(|(header_name, header_value)| {
                    header_value
                        .to_str()
                        .ok()
                        .map(|header_value| format!("{header_name}: {header_value}"))
                })
                .collect::<Vec<_>>(),
        )
    }
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use hyper::Request;
    use mirrord_protocol::tcp::{self, Filter, HttpMethodFilter, JsonPathQuery};

    use super::{HttpFilter, MatchResult, RequestBody};

    /// No body in hand and none coming: what a header-only filter sees at the head.
    const NO_BODY: RequestBody<&[u8]> = RequestBody::Pending;

    fn head(uri: &str, headers: &[(&str, &str)]) -> hyper::http::request::Parts {
        let mut request = Request::builder().uri(uri);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.body(()).unwrap().into_parts().0
    }

    fn body_filter(tenant: &str) -> tcp::HttpFilter {
        tcp::HttpFilter::Body(tcp::HttpBodyFilter::Json {
            query: JsonPathQuery::new("$.tenant".to_owned()).unwrap(),
            matches: Filter::new(tenant.to_owned()).unwrap(),
        })
    }

    fn header_filter(header: &str) -> tcp::HttpFilter {
        tcp::HttpFilter::Header(Filter::new(header.to_owned()).unwrap())
    }

    /// A `Not` filter steals exactly the requests its inner filter leaves alone, so a preview
    /// pod can send everything that is not preview traffic back to the stable pods.
    #[tokio::test]
    async fn not_filter_inverts_its_inner_filter() {
        let tcp_filter =
            tcp::HttpFilter::Not(Box::new(header_filter("baggage: .*mirrord=preview-key")));
        let filter = HttpFilter::try_from(&tcp_filter).unwrap();

        let mut preview = head("/orders", &[("baggage", "mirrord=preview-key")]);
        assert_eq!(
            filter.matches(&mut preview, NO_BODY).await,
            MatchResult::NoMatch
        );

        let mut live = head("/orders", &[]);
        assert_eq!(filter.matches(&mut live, NO_BODY).await, MatchResult::Match);
    }

    /// Mirroring and stealing check a request once before the body is buffered. A `Not` over a
    /// body filter must not read "no body yet" as "body does not match", or the request is
    /// handed over before its body is seen and a body that does match slips through. It asks for
    /// the body instead, and decides once the body is there.
    #[tokio::test]
    async fn not_over_body_filter_asks_for_the_body() {
        let tcp_filter = tcp::HttpFilter::Not(Box::new(body_filter("preview")));
        let filter = HttpFilter::try_from(&tcp_filter).unwrap();

        let mut parts = head("/orders", &[]);
        assert_eq!(
            filter.matches(&mut parts, NO_BODY).await,
            MatchResult::NeedsBody
        );
        assert_eq!(
            filter
                .matches(
                    &mut parts,
                    RequestBody::Complete(br#"{"tenant":"preview"}"#.as_slice())
                )
                .await,
            MatchResult::NoMatch
        );
        assert_eq!(
            filter
                .matches(
                    &mut parts,
                    RequestBody::Complete(br#"{"tenant":"live"}"#.as_slice())
                )
                .await,
            MatchResult::Match
        );
    }

    /// Buffering gives up on a body over the size limit or one that arrives too slowly. A body
    /// filter cannot match a body nobody read, and its negation then matches: the preview pod
    /// sends the request back to the stable pods rather than serving a body it could not check.
    /// Returning "no match" for the negation would leak exactly those requests to the preview
    /// pod's own code.
    #[tokio::test]
    async fn unavailable_body_matches_a_negated_body_filter_only() {
        let negated =
            HttpFilter::try_from(&tcp::HttpFilter::Not(Box::new(body_filter("preview")))).unwrap();
        let plain = HttpFilter::try_from(&body_filter("preview")).unwrap();

        let mut parts = head("/orders", &[]);
        assert_eq!(
            negated
                .matches(&mut parts, RequestBody::<&[u8]>::Unavailable)
                .await,
            MatchResult::Match
        );
        assert_eq!(
            plain
                .matches(&mut parts, RequestBody::<&[u8]>::Unavailable)
                .await,
            MatchResult::NoMatch
        );
    }

    /// `not (all_of [header, body])` on a request without the header: the inner `all_of` is false
    /// from the headers alone, so the negation is true without ever looking at the body. Waiting
    /// for the body here would delay every such request by a full buffer, and lose it entirely
    /// when buffering fails. Only a request that does carry the header needs its body.
    #[tokio::test]
    async fn headers_decide_a_negated_composite_without_the_body() {
        let tcp_filter = tcp::HttpFilter::Not(Box::new(tcp::HttpFilter::Composite {
            all: true,
            filters: vec![header_filter("x-tenant: preview"), body_filter("preview")],
        }));
        let filter = HttpFilter::try_from(&tcp_filter).unwrap();

        let mut without_header = head("/orders", &[]);
        assert_eq!(
            filter.matches(&mut without_header, NO_BODY).await,
            MatchResult::Match
        );

        let mut with_header = head("/orders", &[("x-tenant", "preview")]);
        assert_eq!(
            filter.matches(&mut with_header, NO_BODY).await,
            MatchResult::NeedsBody
        );
    }

    #[tokio::test]
    async fn matching_all_filter() {
        let tcp_filter = tcp::HttpFilter::Composite {
            all: true,
            filters: vec![
                tcp::HttpFilter::Header(Filter::new("brass-key: a-bazillion".to_owned()).unwrap()),
                tcp::HttpFilter::Path(Filter::new("path/to/v1".to_owned()).unwrap()),
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
        assert_eq!(
            filter.matches(&mut input, NO_BODY).await,
            MatchResult::Match
        );

        // should fail
        let mut input = Request::builder()
            .method("POST")
            .uri("https://www.balconia.gov/api/path/to/v1")
            .header("brass-key", "nothin")
            .body(())
            .unwrap()
            .into_parts()
            .0;
        assert_eq!(
            filter.matches(&mut input, NO_BODY).await,
            MatchResult::NoMatch
        );
    }

    #[tokio::test]
    async fn matching_any_filter() {
        let tcp_filter = tcp::HttpFilter::Composite {
            all: false,
            filters: vec![
                tcp::HttpFilter::Header(Filter::new("brass-key: a-bazillion".to_owned()).unwrap()),
                tcp::HttpFilter::Header(Filter::new("dungeon-key: heavy".to_owned()).unwrap()),
                tcp::HttpFilter::Path(Filter::new("path/to/v1".to_owned()).unwrap()),
                tcp::HttpFilter::Path(Filter::new("path/for/v8".to_owned()).unwrap()),
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
        assert_eq!(
            filter.matches(&mut input, NO_BODY).await,
            MatchResult::Match
        );

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
        assert_eq!(
            filter.matches(&mut input, NO_BODY).await,
            MatchResult::NoMatch
        );
    }
}
