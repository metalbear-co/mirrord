// #![warn(missing_docs)]
// #![warn(rustdoc::missing_crate_level_docs)]

use core::convert::Infallible;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use dashmap::DashMap;
use fancy_regex::Regex;
use futures::TryFutureExt;
use hyper::{
    body,
    server::conn::{http1, http2},
    service::service_fn,
    Request, Response,
};
use mirrord_protocol::ConnectionId;
use thiserror::Error;
use tokio::{
    io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream, ReadBuf},
    net::TcpStream,
    sync::mpsc::{channel, Receiver, Sender},
    task::JoinHandle,
};
use tracing::{debug, error};

use crate::{
    error::AgentError,
    util::{run_thread, ClientId},
};

#[derive(Error, Debug)]
pub enum HttpError {
    #[error("Failed parsing HTTP with 0 bytes!")]
    Empty,

    #[error("Failed client not found `{0}`!")]
    ClientNotFound(ClientId),

    #[error("Failed parsing HTTP smaller than minimal!")]
    TooSmall,

    #[error("Failed as the buffer does not contain a valid HTTP request!")]
    NotHttp,

    #[error("Failed with IO `{0}`!")]
    IO(#[from] std::io::Error),

    #[error("Failed with Parse `{0}`!")]
    Parse(#[from] httparse::Error),

    #[error("Failed with Hyper `{0}`!")]
    Hyper(#[from] hyper::Error),
}

const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HttpVersion {
    #[default]
    V1,
    V2,
}

struct HttpFilterBuilder {
    http_version: HttpVersion,
    original_stream: TcpStream,
    hyper_stream: DuplexStream,
    interceptor_stream: DuplexStream,
    filters: Arc<DashMap<ClientId, Regex>>,
}

impl HttpVersion {
    /// Checks if `buffer` contains a valid HTTP/1.x request, or if it could be an HTTP/2 request by
    /// comparing it with a slice of [`H2_PREFACE`].
    fn new(buffer: &[u8], h2_preface: &[u8]) -> Result<Self, HttpError> {
        let mut empty_headers = [httparse::EMPTY_HEADER; 0];

        if buffer == h2_preface {
            Ok(Self::V2)
        } else if matches!(
            httparse::Request::new(&mut empty_headers).parse(buffer),
            Ok(_) | Err(httparse::Error::TooManyHeaders)
        ) {
            Ok(Self::V1)
        } else {
            Err(HttpError::NotHttp)
        }
    }
}

impl HttpFilterBuilder {
    /// Does not consume bytes from the stream.
    ///
    /// Checks if the first available bytes in a stream could be of an http request.
    ///
    /// This is a best effort classification, not a guarantee that the stream is HTTP.
    async fn new(
        tcp_stream: TcpStream,
        filters: Arc<DashMap<ClientId, Regex>>,
    ) -> Result<Self, HttpError> {
        let mut buffer = [0u8; 64];
        // TODO(alex) [mid] 2022-12-09: Maybe we do need `poll_peek` here, otherwise just `peek`
        // might return 0 bytes peeked.
        match tcp_stream
            .peek(&mut buffer)
            .await
            .map_err(From::from)
            .and_then(|peeked_amount| {
                if peeked_amount == 0 {
                    Err(HttpError::Empty)
                } else {
                    Ok(peeked_amount)
                }
            })
            .and_then(|peeked_amount| {
                HttpVersion::new(&buffer[1..peeked_amount], &H2_PREFACE[1..peeked_amount])
            }) {
            Ok(http_version) => {
                let (hyper_stream, interceptor_stream) = duplex(1500);

                Ok(Self {
                    http_version,
                    filters,
                    original_stream: tcp_stream,
                    hyper_stream,
                    interceptor_stream,
                })
            }
            // TODO(alex) [mid] 2022-12-09: This whole filter is a passthrough case.
            Err(HttpError::NotHttp) => todo!(),
            Err(fail) => {
                error!("Something went wrong in http filter {fail:#?}");
                Err(fail)
            }
        }
    }

    /// Creates the hyper task, and returns an [`HttpFilter`] that contains the channels we use to
    /// pass the requests to the layer.
    fn start(self) -> Result<HttpFilter, HttpError> {
        let Self {
            http_version,
            original_stream,
            hyper_stream,
            interceptor_stream,
            filters,
        } = self;

        let hyper_task = match http_version {
            HttpVersion::V1 => tokio::task::spawn(async move {
                let regexes = filters.iter().map(|k| k).collect::<Vec<_>>();

                // TODO(alex) [high] 2022-12-12: We need a channel in the handler to pass the fully
                // built request to the agent (which will be sent to the layer).
                http1::Builder::new()
                    .serve_connection(
                        hyper_stream,
                        service_fn(|request: Request<body::Incoming>| async move {
                            println!("hyper request \n{:#?}", request);
                            Ok::<_, Infallible>(Response::new("hello".to_string()))
                        }),
                    )
                    .await
                    .map_err(From::from)
            }),
            // TODO(alex) [mid] 2022-12-09: http2 builder wants an executor?
            HttpVersion::V2 => {
                http2::Builder::new(todo!());

                todo!()
            }
        };

        Ok(HttpFilter {
            hyper_task,
            original_stream,
            interceptor_stream,
        })
    }
}

// TODO(alex) [mid] 2022-12-12: Wrapper around some of the streams/channels we need to make the
// hyper task and the stealer talk.
//
// Need a `Sender<Request>` channel here I think, that we use inside hypers' `service_fn` to send
// the request to the stealer.
struct HttpFilter {
    hyper_task: JoinHandle<Result<(), HttpError>>,
    /// The original [`TcpStream`] that is connected to us, this is where we receive the requests
    /// from.
    original_stream: TcpStream,

    /// A stream that we use to communicate with the hyper task.
    ///
    /// Don't ever [`DuplexStream::read`] anything from it, as the hyper task only responds with
    /// garbage (treat the `read` side as `/dev/null`).
    ///
    /// We use [`DuplexStream::write`] to write the bytes we have `read` from [`original_stream`]
    /// to the hyper task, acting as a "client".
    interceptor_stream: DuplexStream,
}

/// Created for every new port we want to filter HTTP traffic on.
pub(super) struct HttpFilterManager {
    // TODO(alex) [low] 2022-12-12: Probably don't need this, adding for debugging right now.
    port: u16,
    filters: Arc<DashMap<ClientId, Regex>>,
}

impl HttpFilterManager {
    pub(super) fn new(port: u16, client_id: ClientId, filter: Regex) -> Self {
        let filters = Arc::new(DashMap::with_capacity(128));
        filters.insert(client_id, filter).expect("First insertion!");

        Self { port, filters }
    }

    // TODO(alex) [high] 2022-12-12: Is adding a filter like this enough for it to be added to the
    // hyper task? Do we have a possible deadlock here? Tune in next week for the conclusion!
    pub(super) fn new_client(&mut self, client_id: ClientId, filter: Regex) -> Option<Regex> {
        self.filters.insert(client_id, filter)
    }

    // TODO(alex) [high] 2022-12-12: hyper doesn't take the actual stream, we're going to be
    // separating it in reader/writer, so hyper can just return empty responses to nowhere (we glue
    // a writer from a duplex channel to the actual reader from TcpStream).
    //
    // If it matches the filter, we send this request via a channel to the layer. And on the
    // Manager, we wait for a message from the layer to send on the writer side of the actual
    // TcpStream.
    async fn new_connection(&self, connection: TcpStream) -> Result<(), HttpError> {
        let http_filter = HttpFilterBuilder::new(connection, self.filters.clone())
            .await?
            .start()?;

        todo!()
    }
}
