// #![warn(missing_docs)]
// #![warn(rustdoc::missing_crate_level_docs)]

use core::{convert::Infallible, pin::Pin};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use dashmap::DashMap;
use fancy_regex::Regex;
use futures::{Future, FutureExt, TryFutureExt};
use hyper::{
    body::{self, Incoming},
    server::conn::{http1, http2},
    service::{service_fn, Service},
    Request, Response,
};
use mirrord_protocol::ConnectionId;
use thiserror::Error;
use tokio::{
    io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream, ReadBuf},
    net::TcpStream,
    sync::{
        mpsc::{channel, Receiver, Sender},
        oneshot,
    },
    task::JoinHandle,
};
use tracing::{debug, error};

use crate::{
    error::AgentError,
    util::{run_thread, ClientId},
};

#[derive(Error, Debug)]
pub(super) enum HttpError {
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

    #[error("Failed with Captured `{0}`!")]
    CapturedSender(#[from] tokio::sync::mpsc::error::SendError<CapturedRequest>),

    #[error("Failed with Passthrough `{0}`!")]
    PassthroughSender(#[from] tokio::sync::mpsc::error::SendError<PassthroughRequest>),
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
    client_filters: Arc<DashMap<ClientId, Regex>>,
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

#[derive(Debug)]
pub(super) struct CapturedRequest {
    client_id: ClientId,
    request: Request<Incoming>,
}

#[derive(Debug)]
pub(super) struct PassthroughRequest(Request<Incoming>);

struct HyperFilter {
    filters: Arc<DashMap<ClientId, Regex>>,
    captured_tx: Sender<CapturedRequest>,
    passthrough_tx: Sender<PassthroughRequest>,
}

impl Service<Request<Incoming>> for HyperFilter {
    type Response = Response<String>;

    type Error = HttpError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
        println!("hyper request \n{:#?}", request);
        let captured_tx = self.captured_tx.clone();

        request
            .headers()
            .iter()
            .map(|(header_name, header_value)| {
                format!("{}={}", header_name, header_value.to_str().unwrap())
            })
            .find_map(|header| {
                self.filters.iter().find_map(|filter| {
                    if filter.is_match(&header).unwrap() {
                        Some(filter.key().clone())
                    } else {
                        None
                    }
                })
            })
            .map(move |client_id| {
                let captured = CapturedRequest { client_id, request };

                captured_tx
                    .send(captured)
                    .map_ok(|()| Response::new(format!("")))
                    .map_err(HttpError::from)
                    .into_future()
                    .boxed()
            })
            .unwrap()

        // TODO(alex) [high] 2022-12-13: Now I need to send this data back for the 2 cases:
        //
        // 1. `Some(_)`: send the client_id and the request to the stealer, this request has to
        // reach the layer asap!
        //
        // 2. `None`: not filtered, send this to the agent, and let it handle the passthrough?

        // client_request_with_id

        // Box::pin(async { Ok(Response::new("Boo".to_string())) })
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
                    client_filters: filters,
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

    /*
       key: .*3

       key: value
       key2: value2
    */

    /// Creates the hyper task, and returns an [`HttpFilter`] that contains the channels we use to
    /// pass the requests to the layer.
    fn start(self) -> Result<HttpFilter, HttpError> {
        let Self {
            http_version,
            original_stream,
            hyper_stream,
            interceptor_stream,
            client_filters,
        } = self;

        let (captured_tx, captured_rx) = channel(1500);
        let (passthrough_tx, passthrough_rx) = channel(1500);

        let client_f1 = client_filters.clone();
        let hyper_task = match http_version {
            HttpVersion::V1 => tokio::task::spawn(async move {
                // TODO(alex) [high] 2022-12-12: We need a channel in the handler to pass the
                // fully built request to the agent (which will be sent to
                // the layer).

                // TODO(alex) [high] 2022-12-13: To solve this we can go 2 ways:
                // 1. Use async closures;
                // 2. Closure first, then async move;
                http1::Builder::new()
                    .serve_connection(
                        hyper_stream,
                        HyperFilter {
                            filters: client_filters,
                            captured_tx,
                            passthrough_tx,
                        },
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
            captured_rx,
            passthrough_rx,
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
    captured_rx: Receiver<CapturedRequest>,
    passthrough_rx: Receiver<PassthroughRequest>,
}

/// Created for every new port we want to filter HTTP traffic on.
pub(super) struct HttpFilterManager {
    // TODO(alex) [low] 2022-12-12: Probably don't need this, adding for debugging right now.
    port: u16,
    client_filters: Arc<DashMap<ClientId, Regex>>,
}

impl HttpFilterManager {
    pub(super) fn new(port: u16, client_id: ClientId, filter: Regex) -> Self {
        let client_filters = Arc::new(DashMap::with_capacity(128));
        client_filters
            .insert(client_id, filter)
            .expect("First insertion!");

        Self {
            port,
            client_filters,
        }
    }

    // TODO(alex) [high] 2022-12-12: Is adding a filter like this enough for it to be added to the
    // hyper task? Do we have a possible deadlock here? Tune in next week for the conclusion!
    pub(super) fn new_client(&mut self, client_id: ClientId, filter: Regex) -> Option<Regex> {
        self.client_filters.insert(client_id, filter)
    }

    // TODO(alex) [high] 2022-12-12: hyper doesn't take the actual stream, we're going to be
    // separating it in reader/writer, so hyper can just return empty responses to nowhere (we glue
    // a writer from a duplex channel to the actual reader from TcpStream).
    //
    // If it matches the filter, we send this request via a channel to the layer. And on the
    // Manager, we wait for a message from the layer to send on the writer side of the actual
    // TcpStream.
    async fn new_connection(&self, connection: TcpStream) -> Result<(), HttpError> {
        let http_filter = HttpFilterBuilder::new(connection, self.client_filters.clone())
            .await?
            .start()?;

        todo!()
    }
}
