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

// TODO(alex) [high] 2022-12-06: Serve 1 hyper per connection? If we don't do 1 to 1, then there is
// no easy way of knowing to whom we want to respond, or if these bytes are part of read A or read
// B.

// TODO(alex) [high] 2022-12-06: Which of these 2 do we want?
// - `A` leaves handling the pairing of connection/client id to the agent;
// - `B` makes this crate aware of such associations;
//
// I would go for `A`, and have the agent deal with ports, connections, and stuff like that.
//
// This means that the agent will be creating many filters.
//
// ADD(alex) [high] 2022-12-06: Flow is: agent creates a `Filter` with
// `new() -> Receiver<PassOrCaptured>`, this means that `Filter` holds a `Sender<PassOrCaptured>`.
//
// We use this `Sender` to send the response we get from the duplex channel we create for hyper,
// doing so keeps the whole filtering inside of this crate, and the agent will just keep reading
// from the `Receiver` channel.
//
// The agent-filter message is a bit more involved, we're going to use some sort of `Command`-like
// enum, so we can send back errors, maybe even a "done" message.
//
// We hold the `Client` side of the `DuplexStream` here, and have a public method `filter_message`
// that acts as a `stream.send()` wrapper, so the agent doesn't need to hold anything more than
// the `Filter` itself, and the `Receiver`.
//
// ADD(alex) [high] 2022-12-07: We probably need to hold a map of client/filter(regex) here?
// Not in the `Filter` itself, I think this should be done in the agent? Or in some higher
// abstraction?

// TODO(alex) [low] 2022-12-08: We need to unify some of these types in a common crate, something
// like a `types` crate.

// TODO(alex) [high] 2022-12-07: This is created by the stealer (during its creation phase).
//
// ADD(alex) [high] 2022-12-09: We don't generate any of these `id`s here, we get them from the
// stealer, to have a single global source for them.
pub struct EnterpriseTrafficManager {
    client_filters: HashMap<ClientId, Regex>,
}

struct TrafficClient {
    filter: Regex,
    connections: HashMap<ConnectionId, TcpStream>,
}

impl Default for EnterpriseTrafficManager {
    fn default() -> Self {
        Self {
            client_filters: Default::default(),
        }
    }
}

impl EnterpriseTrafficManager {
    // TODO(alex) [high] 2022-12-07: We don't have connections yet, just a filter for this client,
    // so no hyper involved here.
    //
    // ADD(alex) [high] 2022-12-08: Don't think we even need this, just pass the filter when there
    // is a new connection. Yeah, it could be done there, but then it gets messy if the client wants
    // to add a new filter, or change an existing one?
    //
    // It's less about adding/changing filters, and more about we keep this here instead of keeping
    // the client/filter map in the agent.
    pub fn new_client(&mut self, client_id: ClientId, filter: Regex) {
        self.client_filters.insert(client_id, filter);
    }

    // TODO(alex) [mid] 2022-12-09: Should this be more similar to how stealer port subscription
    // works? If so, we need some sort of `filter_unsubscribe` message, where we remove only 1
    // filter from a client.
    pub fn stop_client(&mut self, client_id: ClientId) {
        self.client_filters.remove(&client_id);

        // TODO(alex) [mid] 2022-12-09: We need to kill the http filter task here as well.
    }

    // TODO(alex) [high] 2022-12-08: agent got a new connection in `listener.accept`, so now we
    // create the hyper connection and filter on it.
    //
    // We must return a channel that will be used by the stealer to send us the actual, final
    // response it gets from the layer, as we're taking the stream.
    //
    // The `Receiver` part of this channel does a `blocking_recv` in the hyper handler.
    // pub async fn new_connection<ResponseFromStolenLayer>(
    //     &mut self,
    //     connection_id: ConnectionId,
    //     tcp_stream: TcpStream,
    // ) -> Result<(), HttpError> {
    //     let filter = self
    //         .client_filters
    //         .get(&client_id)
    //         .ok_or(HttpError::ClientNotFound(client_id))
    //         .cloned()?;

    //     // TODO(alex) [high] 2022-12-09: We check if this is HTTP in the building process.
    //     // - If we get some IO error, then this whole `tcp_stream` is doomed;
    //     // - If the error is just that this is not HTTP, then we create some sort of passthrough
    //     //   filter that will just send/receive messages on the stream, very similar to what
    // happens     //   when a filter doesn't match.
    //     HttpFilter::new(client_id, connection_id, filter, tcp_stream).await?;

    //     todo!()
    // }
}

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

struct HttpFilterBuilder {
    http_version: HttpVersion,
    original_stream: TcpStream,
    hyper_stream: DuplexStream,
    interceptor_stream: DuplexStream,
    filters: Arc<DashMap<ClientId, Regex>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HttpVersion {
    #[default]
    V1,
    V2,
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

    // TODO(alex) [high] 2022-12-08: Creates the hyper task, should return the channels we need to
    // communicate with the hyper task.
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
