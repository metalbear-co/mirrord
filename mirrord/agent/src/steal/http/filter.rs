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

            Self::Path(filter) => {
                let path = request.uri().path();
                filter
                    .is_match(path)
                    .inspect_err(|error| {
                        tracing::error!(path, ?error, "Error while matching path");
                    })
                    .unwrap_or(false)
            }
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
