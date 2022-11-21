#![feature(once_cell)]

use std::sync::OnceLock;

use fancy_regex::Regex;
use httparse::Header;
use mirrord_config::{network::NetworkConfig, util::VecOrSingle};
use thiserror::Error;
use tracing::debug;

// TODO(alex) [mid] 2022-11-17: To parse http2, set up a client/server (hyper) so we can grab the
// request, pass it to ourselves (to this dummy hyper thingy) and then "parse" with hyper.

/// Holds the `Regex` that is used to either continue or bypass file path operations (such as
/// [`file::ops::open`]), according to what the user specified.
///
/// The [`HttpFilter::Include`] variant takes precedence and erases whatever the user supplied as
/// exclude, this means that if the user specifies both, `HttpFilter::Exclude` is never constructed.
///
/// Warning: Use [`HttpFilter::new`] (or equivalent) when initializing this, otherwise the above
/// constraint might not be held.
#[derive(Debug, Clone)]
pub enum HttpFilter {
    /// User specified `Regex` containing the file paths that the user wants to include for file
    /// operations.
    ///
    /// Overrides [`HttpFilter::Exclude`].
    Include(Regex),

    /// Combination of [`DEFAULT_EXCLUDE_LIST`] and the user's specified `Regex`.
    ///
    /// Anything not matched by this `Regex` is considered as included.
    Exclude(Regex),
}

impl From<NetworkConfig> for HttpFilter {
    /// Initializes a `HttpFilter` based on the user configuration.
    ///
    /// - [`HttpFilter::Include`] is returned if the user specified any include path (thus erasing
    ///   anything passed as exclude);
    /// - [`HttpFilter::Exclude`] also appends the [`DEFAULT_EXCLUDE_LIST`] to the user supplied
    ///   regex;
    #[tracing::instrument(level = "debug")]
    fn from(network_config: NetworkConfig) -> Self {
        let NetworkConfig {
            http_include,
            http_exclude,
            ..
        } = network_config;

        let include = http_include.map(VecOrSingle::to_vec).unwrap_or_default();
        let exclude = http_exclude.map(VecOrSingle::to_vec).unwrap_or_default();

        Self::new(include, exclude)
    }
}

impl HttpFilter {
    pub fn new(include: Vec<String>, exclude: Vec<String>) -> Self {
        let default_include = ".*";
        let default_include_regex =
            Regex::new(default_include).expect("Failed parsing default exclude file regex!");

        // Converts a list of `String` into one big regex-fied `String`.
        let reduce_to_string = |list: Vec<String>| {
            list.into_iter()
                // Turn into capture group `(/folder/first.txt)`.
                .map(|element| format!("({element})"))
                // Put `or` operation between groups `(/folder/first.txt)|(/folder/second.txt)`.
                .reduce(|acc, element| format!("{acc}|{element}"))
        };

        let exclude = reduce_to_string(exclude)
            .as_deref()
            .map(Regex::new)
            .transpose()
            .expect("Failed parsing include http regex!")
            .map(Self::Exclude);

        // Try to generate the final `Regex` based on `include`.
        reduce_to_string(include)
            .as_deref()
            .map(Regex::new)
            .transpose()
            .expect("Failed parsing include file regex!")
            .map(Self::Include)
            // `include` was empty, so we fallback to `exclude`.
            .or(exclude)
            // `exclude` was also empty, so we fallback to `default_include`.
            .unwrap_or(Self::Include(default_include_regex))
    }

    // TODO(alex) [mid] 2022-11-21: Somewhat garbage API just to avoid putting a fancy-regex
    // dependency in agent, or a mirrrod-protocol dependency here.
    pub fn new_include(include: String) -> Self {
        Self::Include(Regex::new(&include).expect("Include should already be valid here!"))
    }

    // TODO(alex) [mid] 2022-11-21: Somewhat garbage API just to avoid putting a fancy-regex
    // dependency in agent, or a mirrrod-protocol dependency here.
    pub fn new_exclude(exclude: String) -> Self {
        Self::Exclude(Regex::new(&exclude).expect("Exclude should already be valid here!"))
    }

    fn matches(&self, text: &str) -> bool {
        match self {
            HttpFilter::Include(include) if include.is_match(text).unwrap() => true,
            HttpFilter::Exclude(exclude) if !exclude.is_match(text).unwrap() => true,
            _ => false,
        }
    }

    #[tracing::instrument(level = "debug", skip(self, bytes))]
    pub fn capture_packet(&self, bytes: &[u8]) -> Result<bool, HttpError> {
        let headers = parse_headers(bytes)?;
        Ok(headers.into_iter().any(|header| self.matches(&header)))
    }
}
// TODO(alex) [high] 2022-11-18: The biggest challenge here is that the agent has to know which
// filter to apply, and there is no way of passing different configs to it (we're currently using
// env vars to do the agent config).
//
// So the simplest way is to pass the filter from `layer` with a message, similar to how
// `StealSubscribe` works, so we send a `SetFilter` message that initializes the whole thing.
//
// Meaning we should probably keep the `HttpFilter` in the `TcpStealHandler` (agent), and allow it
// to be dynamically changed with this `SetFilter` message.
//
// With this we could also create an association between filter + who to send this message to.

impl Default for HttpFilter {
    fn default() -> Self {
        Self::Include(Regex::new(&".*").unwrap())
    }
}

#[derive(Error, Debug)]
pub enum HttpError {
    #[error("Failed parsing HTTP with 0 bytes!")]
    Empty,

    #[error("Failed parsing HTTP smaller than minimal!")]
    TooSmall,

    #[error("Failed parsing HTTP with `{0}`!")]
    HttParse(#[from] httparse::Error),
}

const MINIMAL_HTTP1_REQUEST: &str = "GET / HTTP/1.1";

const fn validate_length(length: usize) -> Result<(), HttpError> {
    if length == 0 {
        Err(HttpError::Empty)
    } else if length < MINIMAL_HTTP1_REQUEST.len() {
        Err(HttpError::TooSmall)
    } else {
        Ok(())
    }
}

fn parse_headers(bytes: &[u8]) -> Result<Vec<String>, HttpError> {
    validate_length(bytes.len())?;

    let mut headers = vec![httparse::EMPTY_HEADER; 32];
    let mut request = httparse::Request::new(&mut headers);
    debug!("request initial {:#?}", request);

    // Fills the `headers`: `{ name: User-Agent, value: [bytes] }`.
    let request = request.parse(&bytes)?;
    debug!("request {:#?}", request);
    debug!("headers {:#?}", headers);

    // Converts the headers into: `User-Agent:curl/7.86.0`.
    let headers = headers
        .into_iter()
        .map(|header| format!("{}:{}", header.name, String::from_utf8_lossy(header.value)))
        .collect();

    Ok(headers)
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

    use super::*;

    const HTTP1_SAMPLE: &str =
        "GET / HTTP/1.1\r\nHost: localhost:30000\r\nUser-Agent: curl/7.68.0\r\nAccept: */*\r\n\r\n";

    const HTTP1_BIG_REQUEST: &str = "POST / HTTP/1.1\r\nHost: localhost:30000\r\nUser-Agent: curl/7.68.0\r\nAccept: */*\r\nContent-Length: 1975\r\nContent-Type: application/x-www-form-urlencoded\r\nExpect: 100-continue\r\n\r\n";

    fn tracing_start() {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_thread_ids(true)
                    .with_span_events(FmtSpan::ACTIVE)
                    .compact(),
            )
            .with(tracing_subscriber::EnvFilter::from_default_env())
            .init();
    }

    #[test]
    #[should_panic]
    fn test_panic_empty_buffer() {
        parse_headers("".as_bytes()).unwrap();
    }

    #[test]
    #[should_panic]
    fn test_panic_http1_invalid() {
        parse_headers("GET / HTTP/7.0".as_bytes()).unwrap();
    }

    #[test]
    fn test_parse_http1_minimal() {
        parse_headers(MINIMAL_HTTP1_REQUEST.as_bytes()).unwrap();
    }

    #[test]
    fn test_parse_http1_sample() {
        parse_headers(HTTP1_SAMPLE.as_bytes()).unwrap();
    }

    #[test]
    fn test_parse_http1_big() {
        tracing_start();
        parse_headers(HTTP1_BIG_REQUEST.as_bytes()).unwrap();
    }
}
