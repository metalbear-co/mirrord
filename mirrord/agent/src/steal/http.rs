//! Home for most of the HTTP stealer implementation / modules.
//!
//! # [`HttpV`]
//!
//! Helper trait to deal with [`hyper`] differences betwen HTTP/1 and HTTP/2 types.
//!
//! # [`HttpVersion`]
//!
//! # [`HttpFilterManager`]
//!
//! Holds the filters for a port we're stealing HTTP traffic on.
use std::{net::SocketAddr, sync::Arc};

use bytes::Bytes;
use dashmap::DashMap;
use fancy_regex::Regex;
use http_body_util::Full;
use hyper::{
    body::Incoming,
    server::{
        self,
        conn::{http1, http2},
    },
    Request, Response,
};
use mirrord_protocol::ConnectionId;
use tokio::{
    io::{copy_bidirectional, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc::Sender, oneshot},
};
use tracing::{error, trace};

use self::{
    error::HttpTrafficError,
    filter::{close_connection, TokioExecutor, MINIMAL_HEADER_SIZE},
    hyper_handler::{HyperHandler, RawHyperConnection},
    reversible_stream::ReversibleStream,
    v1::HttpV1,
    v2::HttpV2,
};
use crate::{
    steal::{http::filter::filter_task, HandlerHttpRequest},
    util::ClientId,
};

pub(crate) mod error;
mod filter;
mod hyper_handler;
mod reversible_stream;
mod v1;
mod v2;

/// Handy alias due to [`ReversibleStream`] being generic, avoiding value mismatches.
type DefaultReversibleStream = ReversibleStream<MINIMAL_HEADER_SIZE>;

/// Unifies [`hyper`] handling for HTTP/1 and HTTP/2.
///
/// # Details
///
/// As most of the `hyper` types around HTTP/1 and HTTP/2 are different, and do not share a trait,
/// we use [`HttpV`] to create a shared implementation that is used by
/// [`hyper_handler::HyperHandler`].
trait HttpV {
    /// Type for hyper's `SendRequest`.
    ///
    /// It's a different type for HTTP/1 ([`http1::SendRequest`]) and
    /// HTTP/2 ([`http2::SendRequest`]).
    ///
    /// [`http1::SendRequest`]: hyper::client::conn::http1::SendRequest
    /// [`http2::SendRequest`]: hyper::client::conn::http2::SendRequest
    type Sender;

    /// Performs a client handshake with `target_stream`, creating an HTTP connection.
    ///
    /// # HTTP/1
    ///
    /// The HTTP/1 connection is a bit more involved, as we have to deal with potential `UPGRADE`
    /// requests.
    ///
    /// We do this manually, by keeping the connection alive with `poll_without_shutdown`, and by
    /// sending it as a [`RawHyperConnection`] through `upgrade_tx` to the HTTP stealer handler.
    async fn connect(
        target_stream: TcpStream,
        upgrade_tx: Option<oneshot::Sender<RawHyperConnection>>,
    ) -> Result<Self::Sender, HttpTrafficError>;

    /// Sends the request to the original destination with [`HttpV::Sender`].
    async fn send_request(
        sender: &mut Self::Sender,
        request: Request<Incoming>,
    ) -> Result<Response<Full<Bytes>>, HttpTrafficError>;

    /// Returns `true` if this [`Request`] contains an `UPGRADE` header.
    ///
    /// # Warning:
    ///
    /// This implementation should **always** return `false` for HTTP/2.
    fn is_upgrade(r: &Request<Incoming>) -> bool;
}

/// Identifies a message as being HTTP or not.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HttpVersion {
    /// HTTP/1
    #[default]
    V1,

    /// HTTP/2
    V2,

    /// Handled as a special passthrough case, where the captured stream just forwards messages to
    /// their original destination (and vice-versa).
    NotHttp,
}

impl HttpVersion {
    /// Checks if `buffer` contains a valid HTTP/1.x request, or if it could be an HTTP/2 request by
    /// comparing it with a slice of [`H2_PREFACE`].
    #[tracing::instrument(level = "trace")]
    fn new(buffer: &[u8], h2_preface: &[u8]) -> Self {
        let mut empty_headers = [httparse::EMPTY_HEADER; 0];

        if buffer.len() < MINIMAL_HEADER_SIZE {
            Self::NotHttp
        } else if buffer == h2_preface {
            Self::V2
        } else if matches!(
            httparse::Request::new(&mut empty_headers).parse(buffer),
            Ok(_) | Err(httparse::Error::TooManyHeaders)
        ) {
            Self::V1
        } else {
            Self::NotHttp
        }
    }

    /// Handles the HTTP connection accordingly to its version.
    ///
    /// See [`filter_task`] for more details.
    #[tracing::instrument(level = "trace")]
    async fn serve_connection(
        self,
        mut stream: DefaultReversibleStream,
        original_destination: SocketAddr,
        connection_id: ConnectionId,
        filters: Arc<DashMap<ClientId, Regex>>,
        matched_tx: Sender<HandlerHttpRequest>,
        connection_close_sender: Sender<ConnectionId>,
    ) -> Result<(), HttpTrafficError> {
        match self {
            HttpVersion::V1 => {
                // Contains the upgraded interceptor connection, if any.
                let (upgrade_tx, upgrade_rx) = oneshot::channel::<RawHyperConnection>();

                // We have to keep the connection alive to handle a possible upgrade request
                // manually.
                let server::conn::http1::Parts {
                    io: mut client_agent, // i.e. browser-agent connection
                    read_buf: agent_unprocessed,
                    ..
                } = http1::Builder::new()
                    .preserve_header_case(true)
                    .serve_connection(
                        stream,
                        HyperHandler::<HttpV1>::new(
                            filters,
                            matched_tx,
                            connection_id,
                            original_destination.port(),
                            original_destination,
                            Some(upgrade_tx),
                        ),
                    )
                    .without_shutdown()
                    .await?;

                if let Ok(RawHyperConnection {
                    stream: mut agent_remote, // i.e. agent-original destination connection
                    unprocessed_bytes: client_unprocessed,
                }) = upgrade_rx.await
                {
                    // Send the data we received from the client, and have not processed as
                    // HTTP, to the original destination.
                    agent_remote.write_all(&agent_unprocessed).await?;

                    // Send the data we received from the original destination, and have not
                    // processed as HTTP, to the client.
                    client_agent.write_all(&client_unprocessed).await?;

                    // Now both the client and original destinations should be in sync, so we
                    // can just copy the bytes from one into the other.
                    copy_bidirectional(&mut client_agent, &mut agent_remote).await?;
                }

                close_connection(connection_close_sender, connection_id).await
            }
            HttpVersion::V2 => {
                http2::Builder::new(TokioExecutor::default())
                    .serve_connection(
                        stream,
                        HyperHandler::<HttpV2>::new(
                            filters,
                            matched_tx,
                            connection_id,
                            original_destination.port(),
                            original_destination,
                        ),
                    )
                    .await?;

                close_connection(connection_close_sender, connection_id).await
            }
            HttpVersion::NotHttp => {
                trace!(
                    "Got a connection with unsupported protocol version, passing it through \
                        to its original destination."
                );
                match TcpStream::connect(original_destination).await {
                    Ok(mut interceptor_to_original) => {
                        match copy_bidirectional(&mut stream, &mut interceptor_to_original).await {
                            Ok((incoming, outgoing)) => {
                                trace!(
                                    "Forwarded {incoming} incoming bytes and {outgoing} \
                                        outgoing bytes in passthrough connection"
                                );

                                Ok(())
                            }
                            Err(err) => {
                                error!(
                                    "Encountered error while forwarding unsupported \
                                    connection to its original destination: {err:?}"
                                );

                                Err(err)?
                            }
                        }
                    }
                    Err(err) => {
                        error!(
                            "Could not connect to original destination {original_destination:?}\
                                 . Received a connection with an unsupported protocol version to a \
                                 filtered HTTP port, but cannot forward the connection because of \
                                 the connection error: {err:?}"
                        );
                        Err(err)?
                    }
                }
            }
        }
    }
}

/// Created for every new port we want to filter HTTP traffic on.
#[derive(Debug)]
pub(super) struct HttpFilterManager {
    /// Filters that we're going to be matching against (specified by the user).
    client_filters: Arc<DashMap<ClientId, Regex>>,

    /// We clone this to pass them down to the hyper tasks.
    matched_tx: Sender<HandlerHttpRequest>,
}

impl HttpFilterManager {
    /// Creates a new [`HttpFilterManager`] per port.
    ///
    /// You can't create just an empty [`HttpFilterManager`], as we don't steal traffic on ports
    /// that no client has registered interest in.
    #[tracing::instrument(level = "trace", skip(matched_tx))]
    pub(super) fn new(
        client_id: ClientId,
        filter: Regex,
        matched_tx: Sender<HandlerHttpRequest>,
    ) -> Self {
        let client_filters = Arc::new(DashMap::with_capacity(128));
        client_filters.insert(client_id, filter);

        Self {
            client_filters,
            matched_tx,
        }
    }

    /// Inserts a new client (layer) and its filter.
    ///
    /// [`HttpFilterManager::client_filters`] are shared between hyper tasks, so adding a new one
    /// here will impact the tasks as well.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn add_client(&mut self, client_id: ClientId, filter: Regex) -> Option<Regex> {
        self.client_filters.insert(client_id, filter)
    }

    /// Removes a client (layer) from [`HttpFilterManager::client_filters`].
    ///
    /// [`HttpFilterManager::client_filters`] are shared between hyper tasks, so removing a client
    /// here will impact the tasks as well.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn remove_client(&mut self, client_id: &ClientId) -> Option<(ClientId, Regex)> {
        self.client_filters.remove(client_id)
    }

    /// Checks if we have a filter for this `client_id`.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn contains_client(&self, client_id: &ClientId) -> bool {
        self.client_filters.contains_key(client_id)
    }

    /// Start a [`filter_task`] to handle this new connection.
    #[tracing::instrument(level = "trace", skip(self, original_stream, connection_close_sender))]
    pub(super) async fn new_connection(
        &self,
        original_stream: TcpStream,
        original_address: SocketAddr,
        connection_id: ConnectionId,
        connection_close_sender: Sender<ConnectionId>,
    ) {
        tokio::spawn(filter_task(
            original_stream,
            original_address,
            connection_id,
            self.client_filters.clone(),
            self.matched_tx.clone(),
            connection_close_sender,
        ));
    }

    /// Used by [`TcpConnectionStealer::port_unsubscribe`] to check if we have remaining subscribers
    /// or not.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn is_empty(&self) -> bool {
        self.client_filters.is_empty()
    }
}
