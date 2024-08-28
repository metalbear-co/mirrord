use fancy_regex::Regex;
use hyper::Request;

/// Currently supported filtering criterias.
#[derive(Debug)]
pub enum HttpFilter {
    /// Header based filter.
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
}

impl TryFrom<&mirrord_protocol::tcp::HttpFilter> for HttpFilter {
    type Error = fancy_regex::Error;

    fn try_from(filter: &mirrord_protocol::tcp::HttpFilter) -> Result<Self, Self::Error> {
        match filter {
            mirrord_protocol::tcp::HttpFilter::Header(header) => {
                Ok(Self::Header(Regex::new(&format!("(?i){header}"))?))
            }
            mirrord_protocol::tcp::HttpFilter::Path(path) => {
                Ok(Self::Path(Regex::new(&format!("(?i){path}"))?))
            }
            mirrord_protocol::tcp::HttpFilter::Composite { all, filters } => {
                let all = *all;
                let filters = filters
                    .iter()
                    .map(HttpFilter::try_from)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Self::Composite { all, filters })
            }
        }
    }
}

impl HttpFilter {
    /// Checks whether the given [`Request`] matches this filter.
    pub fn matches<T>(&self, request: &mut Request<T>) -> bool {
        match self {
            Self::Header(filter) => {
                let headers = match request.extensions().get::<NormalizedHeaders>() {
                    Some(cached) => cached,
                    None => {
                        let normalized = request
                            .headers()
                            .iter()
                            .filter_map(|(header_name, header_value)| {
                                header_value
                                    .to_str()
                                    .ok()
                                    .map(|header_value| format!("{header_name}: {header_value}"))
                            })
                            .collect::<Vec<_>>();

                        request
                            .extensions_mut()
                            .insert(NormalizedHeaders(normalized));
                        request
                            .extensions()
                            .get()
                            .expect("extension was just inserted")
                    }
                };

                headers.has_match(filter)
            }

            Self::Path(filter) => request
                .uri()
                .clone()
                .into_parts()
                .path_and_query
                .as_ref()
                .map(|path_and_query| {
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
                .unwrap_or(false),

            Self::Composite { all: true, filters } => filters.iter().all(|f| f.matches(request)),
            Self::Composite {
                all: false,
                filters,
            } => filters.iter().any(|f| f.matches(request)),
        }
    }
}

/// [`HeaderMap`](hyper::http::header::HeaderMap) entries formatted like `k: v` (format expected by
/// [`HttpFilter::Header`]). Computed and cached in [`Request::extensions`] the first time
/// [`HttpFilter::matches`] is called on the [`Request`].
#[derive(Clone)]
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
                .unwrap_or(false)
        })
    }
}

#[cfg(test)]
mod test {
    use hyper::Request;
    use mirrord_protocol::tcp::{self, Filter};

    use crate::steal::http::HttpFilter;

    #[test]
    fn matching_all_filter() {
        let tcp_filter = tcp::HttpFilter::Composite {
            all: true,
            filters: vec![
                tcp::HttpFilter::Header(Filter::new("brass-key: a-bazillion".to_string()).unwrap()),
                tcp::HttpFilter::Path(Filter::new("path/to/v1".to_string()).unwrap()),
            ],
        };

        // should match
        let mut input = Request::builder()
            .uri("https://www.balconia.gov/api/path/to/v1")
            .header("brass-key", "a-bazillion")
            .body(())
            .unwrap();
        let filter: HttpFilter = TryFrom::try_from(&tcp_filter).unwrap();
        assert!(filter.matches(&mut input));

        // should fail
        let mut input = Request::builder()
            .uri("https://www.balconia.gov/api/path/to/v1")
            .header("brass-key", "nothin")
            .body(())
            .unwrap();
        assert!(!filter.matches(&mut input));
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
            ],
        };

        // should match
        let mut input = Request::builder()
            .uri("https://www.balconia.gov/api/path/to/v1")
            .header("brass-key", "nothin")
            .body(())
            .unwrap();
        let filter: HttpFilter = TryFrom::try_from(&tcp_filter).unwrap();
        assert!(filter.matches(&mut input));

        // should fail
        let mut input = Request::builder()
            .uri("https://www.balconia.gov/api/path/to/v3")
            .header("brass-key", "nothin")
            .body(())
            .unwrap();
        let filter: HttpFilter = TryFrom::try_from(&tcp_filter).unwrap();
        assert!(!filter.matches(&mut input));
    }
}
