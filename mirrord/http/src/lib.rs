#![feature(result_option_inspect)]

use core::convert::Infallible;

use fancy_regex::Regex;
use hyper::{body, server::conn::http1, service::service_fn, Request, Response};
use mirrord_protocol::tcp::RegexFilter;
use thiserror::Error;
use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};

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

static INCLUDE_ALL: &str = ".*";

impl Default for HttpFilter {
    fn default() -> Self {
        // TODO(alex): Not a good UX, this should be excluding everything by default (but it would
        // be a breaking change).
        Self::Include(Regex::new(INCLUDE_ALL).unwrap())
    }
}

impl HttpFilter {
    #[tracing::instrument(level = "debug")]
    pub fn new(include: Vec<String>, exclude: Vec<String>) -> Self {
        let default_include_regex =
            Regex::new(INCLUDE_ALL).expect("Failed parsing default exclude file regex!");

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

    #[tracing::instrument(level = "debug")]
    fn matches(&self, text: &str) -> bool {
        match self {
            HttpFilter::Include(include) if include.is_match(text).unwrap() => true,
            HttpFilter::Exclude(exclude) if !exclude.is_match(text).unwrap() => true,
            _ => false,
        }
    }
}

// TODO(alex) [low] 2022-11-25: Should be `TryFrom`, to prevent `unwrap` of invalid values. I can't
// see a way of guaranteeing that we're always the ones creating these regexes from layer config.
impl From<RegexFilter> for HttpFilter {
    fn from(regex_filter: RegexFilter) -> Self {
        match regex_filter {
            RegexFilter::Include(regex_string) => Self::Include(Regex::new(&regex_string).unwrap()),
            RegexFilter::Exclude(regex_string) => Self::Exclude(Regex::new(&regex_string).unwrap()),
        }
    }
}

impl From<HttpFilter> for RegexFilter {
    fn from(http_filter: HttpFilter) -> Self {
        match http_filter {
            HttpFilter::Include(regex) => Self::Include(regex.to_string()),
            HttpFilter::Exclude(regex) => Self::Exclude(regex.to_string()),
        }
    }
}

#[derive(Error, Debug)]
pub enum HttpError {
    #[error("Failed parsing HTTP with 0 bytes!")]
    Empty,

    #[error("Failed parsing HTTP smaller than minimal!")]
    TooSmall,

    #[error("Failed with IO `{0}`!")]
    IO(#[from] std::io::Error),

    #[error("Failed with Parse `{0}`!")]
    Parse(#[from] httparse::Error),

    #[error("Failed with Hyper `{0}`!")]
    Hyper(#[from] hyper::Error),

    #[error("Failed with JoinError `{0}`!")]
    Join(#[from] tokio::task::JoinError),
}

#[derive(Debug)]
pub struct HttpProxy {
    filter: HttpFilter,
    client: DuplexStream,
}

// TODO(alex) [high] 2022-11-28: The packets we capture, can be sent to the layer as `TcpData` to
// be handled by the mirror socket -> user socket, via `ConnectionId`.
impl HttpProxy {
    #[tracing::instrument(level = "debug")]
    pub fn new(client: DuplexStream) -> Self {
        Self {
            client,
            filter: HttpFilter::default(),
        }
    }

    #[tracing::instrument(level = "debug")]
    pub async fn start(server: DuplexStream) -> Result<(), HttpError> {
        let proxy_task = tokio::task::spawn(async move {
            let http1_connection = http1::Builder::new()
                .serve_connection(
                    server,
                    service_fn(|request: Request<body::Incoming>| async move {
                        // TODO(alex) [high] 2022-11-25: Inspect the request, if it should be
                        // captured, then return it in some wrapper type
                        // that indicates this. Otherwise, insert the
                        // request into the response body, then extract it from
                        // the `client.body` (valid for both, as we don't want responses, only
                        // requests).
                        //
                        // ADD(alex) [high] 2022-11-28: Both will be inserted into the body of a
                        // `Response`, so we need to differentiate between those somehow (maybe add
                        // a special header to the "captured" request, and
                        // check for it).
                        Ok::<_, Infallible>(Response::new(request))
                    }),
                )
                .await;

            Ok::<_, HttpError>(http1_connection?)
        });

        proxy_task.await??;

        todo!()
    }
}

const MINIMAL_HTTP1_REQUEST: &str = "GET / HTTP/1.1";

/// Checks if the length of a request is of a valid size.
const fn valid_http1_length(length: usize) -> Result<(), HttpError> {
    // TODO(alex): `length == 0` should also be checked in HTTP/2.
    if length == 0 {
        Err(HttpError::Empty)
    } else if length < MINIMAL_HTTP1_REQUEST.len() {
        Err(HttpError::TooSmall)
    } else {
        Ok(())
    }
}

/// Checks if `bytes` contains a _mostly_ valid HTTP/1 request.
#[tracing::instrument(level = "debug", fields(length = %bytes.len()))]
fn valid_http1_request(bytes: &[u8]) -> Result<(), HttpError> {
    use httparse::*;

    valid_http1_length(bytes.len()).and_then(|()| {
        match Request::new(&mut [EMPTY_HEADER; 0]).parse(&bytes[..]) {
            // Ignore error that occurs due to having more headers than the amount allocated.
            Ok(_) | Err(Error::TooManyHeaders) => Ok(()),
            Err(fail) => Err(fail)?,
        }
    })
}

// TODO(alex) [mid] 2022-11-25: To deal with regex intersection checking
// (avoid 2 users intercepting the same requests?)
// see https://users.rust-lang.org/t/detect-regex-conflict/57184/13
//
// ADD(alex) [mid] 2022-11-25: There is also the trouble of user "X" includes "user: A",
// but user "Y" is excluding as "user: !C", which would capture "A, B" (what "X" wants to capture).
//
// ADD(alex) [mid] 2022-11-28: Solvable by duplicating the traffic, but do we want that?
#[tracing::instrument(level = "debug", fields(length = %bytes.len()))]
pub async fn hyper_debug(bytes: &[u8]) -> Result<(), HttpError> {
    valid_http1_request(bytes)?;

    let (mut client, server) = duplex(12345);

    client.write(bytes).await.unwrap();

    let foo = tokio::task::spawn(async move {
        let wat = http1::Builder::new()
            .serve_connection(
                server,
                service_fn(|request: Request<body::Incoming>| async move {
                    // TODO(alex) [high] 2022-11-25: Inspect the request, if it should be captured,
                    // then return it in some wrapper type that indicates this.
                    // Otherwise, insert the request into the response body, then extract it from
                    // the `client.body` (valid for both, as we don't want responses, only
                    // requests).
                    //
                    // ADD(alex) [high] 2022-11-28: Both will be inserted into the body of a
                    // `Response`, so we need to differentiate between those somehow (maybe add a
                    // special header to the "captured" request, and check for it).
                    println!("foo");
                    Ok::<_, Infallible>(Response::new("hello".to_string()))
                }),
            )
            .await
            .unwrap();
    })
    .await;

    let mut client_buffer = vec![0; 12345];
    let amount = client.read(&mut client_buffer).await.unwrap();
    println!(
        "client {:#?} amount {:#?}",
        String::from_utf8_lossy(&client_buffer[..amount]),
        amount
    );

    println!("foo {foo:#?}");

    Ok(todo!())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const HTTP1_SAMPLE: &str =
        "GET / HTTP/1.1\r\nHost: localhost:30000\r\nUser-Agent: curl/7.68.0\r\nAccept: */*\r\n\r\n";

    const HTTP1_BIG_REQUEST: &str = "POST / HTTP/1.1\r\nHost: localhost:30000\r\nUser-Agent: curl/7.68.0\r\nAccept: */*\r\nContent-Length: 1975\r\nContent-Type: application/x-www-form-urlencoded\r\nExpect: 100-continue\r\n\r\n";

    #[rstest]
    #[case(MINIMAL_HTTP1_REQUEST.as_bytes())]
    #[case(HTTP1_SAMPLE.as_bytes())]
    #[case(HTTP1_BIG_REQUEST.as_bytes())]
    fn test_valid_http1_request(#[case] request: &[u8]) {
        assert!(valid_http1_request(request).is_ok());
    }

    #[rstest]
    #[case("".as_bytes())]
    #[case("I am not an HTTP/1 request, so this should not work!".as_bytes())]
    #[case("GET / HTTP".as_bytes())]
    #[case("GET".as_bytes())]
    fn panic_on_invalid_http1_request(#[case] request: &[u8]) {
        assert!(valid_http1_request(request).is_err());
    }

    // #[tokio::test]
    // async fn traffic_hyper() {
    //     hyper_debug(HTTP1_BIG_REQUEST.as_bytes()).await.unwrap();
    // }
}
