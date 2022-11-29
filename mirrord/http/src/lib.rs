#![feature(result_option_inspect)]
#![warn(missing_docs)]
#![warn(rustdoc::missing_crate_level_docs)]

use core::convert::Infallible;

use fancy_regex::Regex;
use hyper::{body, server::conn::http1, service::service_fn, Request, Response};
use mirrord_protocol::tcp::RegexFilter;
use thiserror::Error;
use tokio::{
    io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream},
    sync::mpsc::{channel, Receiver, Sender},
};

static SELECT_ALL: &str = ".*";

#[derive(Debug, Clone)]
pub struct HttpHeaderSelect {
    /// Matches on header name.
    name: Regex,

    /// Matches on header value.
    value: Regex,
}

impl HttpHeaderSelect {
    #[tracing::instrument(level = "debug")]
    pub fn new(header_name: &str, header_value: &str) -> Self {
        Self {
            name: Regex::new(header_name).unwrap(),
            value: Regex::new(&header_value).unwrap(),
        }
    }
}

// TODO(alex) [low] 2022-11-25: Should be `TryFrom`, to prevent `unwrap` of invalid values. I can't
// see a way of guaranteeing that we're always the ones creating these regexes from layer config.
impl From<RegexFilter> for HttpHeaderSelect {
    fn from(RegexFilter(name, value): RegexFilter) -> Self {
        Self::new(&name, &value)
    }
}

impl From<HttpHeaderSelect> for RegexFilter {
    fn from(HttpHeaderSelect { name, value }: HttpHeaderSelect) -> Self {
        Self(name.to_string(), value.to_string())
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

    #[error("Failed with Sender `{0}`!")]
    Sender(#[from] tokio::sync::mpsc::error::SendError<HttpHeaderSelect>),
}

#[derive(Debug)]
pub struct HttpProxy {
    filter: HttpHeaderSelect,
    client: DuplexStream,
    filter_tx: Sender<HttpHeaderSelect>,
}

// TODO(alex) [high] 2022-11-28: The packets we capture, can be sent to the layer as `TcpData` to
// be handled by the mirror socket -> user socket, via `ConnectionId`.
impl HttpProxy {
    #[tracing::instrument(level = "debug")]
    pub fn new(client: DuplexStream, filter_tx: Sender<HttpHeaderSelect>) -> Self {
        Self {
            client,
            filter_tx,
            filter: HttpHeaderSelect::default(),
        }
    }

    #[tracing::instrument(level = "debug")]
    pub async fn filter(&mut self, filter: HttpHeaderSelect) -> Result<(), HttpError> {
        Ok(self.filter_tx.send(filter).await?)
    }

    #[tracing::instrument(level = "debug")]
    pub async fn start(
        server: DuplexStream,
        filter_rx: Receiver<HttpHeaderSelect>,
    ) -> Result<(), HttpError> {
        let proxy_task = tokio::task::spawn(async move {
            // TODO(alex) [high] 2022-11-28: Use the filter we have from `filter_rx`.
            // Do we need a `select!` here? We need a `loop`.
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
                    request.headers().iter().map(|(x, y)| todo!());
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
