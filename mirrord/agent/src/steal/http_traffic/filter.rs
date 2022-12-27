use std::{net::SocketAddr, sync::Arc};

use dashmap::DashMap;
use fancy_regex::Regex;
use hyper::server::conn::http1;
use mirrord_protocol::ConnectionId;
use tokio::{
    io::{copy_bidirectional, duplex, DuplexStream},
    net::TcpStream,
    sync::mpsc::Sender,
    task::JoinHandle,
};
use tracing::error;

use super::{
    error::HttpTrafficError, hyper_handler::HyperHandler, DefaultReversibleStream, HttpVersion,
    UnmatchedSender,
};
use crate::{
    steal::{http_traffic::error, HandlerHttpRequest, MatchedHttpRequest},
    util::ClientId,
};

const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0";

/// Controls the amount of data we read when trying to detect if the stream's first message contains
/// an HTTP request.
///
/// **WARNING**: Can't be too small, otherwise we end up accepting things like "Foo " as valid HTTP
/// requests.
pub(super) const MINIMAL_HEADER_SIZE: usize = 10;

#[derive(Debug)]
pub(super) struct HttpFilterBuilder {
    http_version: HttpVersion,
    reversible_stream: DefaultReversibleStream,
    original_destination: SocketAddr,
    connection_id: ConnectionId,
    client_filters: Arc<DashMap<ClientId, Regex>>,
    matched_tx: Sender<HandlerHttpRequest>,
    unmatched_tx: UnmatchedSender,

    /// For informing the stealer task that the connection was closed.
    connection_close_sender: Sender<ConnectionId>,
}

/// Used by the stealer handler to:
///
/// 1. Read the requests from hyper's channels through [`matched_rx`], and [`passthrough_rx`];
/// 2. Send the raw bytes we got from the remote connection to hyper through [`interceptor_stream`];
pub(crate) struct HttpFilter {
    pub(super) _hyper_task: JoinHandle<Result<(), HttpTrafficError>>,
    /// The original [`TcpStream`] that is connected to us, this is where we receive the requests
    /// from.
    pub(crate) reversible_stream: DefaultReversibleStream,

    /// A stream that we use to communicate with the hyper task.
    ///
    /// Don't ever [`DuplexStream::read`] anything from it, as the hyper task only responds with
    /// garbage (treat the `read` side as `/dev/null`).
    ///
    /// We use [`DuplexStream::write`] to write the bytes we have `read` from [`original_stream`]
    /// to the hyper task, acting as a "client".
    pub(crate) interceptor_stream: DuplexStream,
}

impl HttpFilterBuilder {
    /// Does not consume bytes from the stream.
    ///
    /// Checks if the first available bytes in a stream could be of an http request.
    ///
    /// This is a best effort classification, not a guarantee that the stream is HTTP.
    #[tracing::instrument(
        level = "debug",
        skip(stolen_stream, matched_tx, unmatched_tx)
        fields(
            local = ?stolen_stream.local_addr(),
            peer = ?stolen_stream.peer_addr()
        ),
    )]
    pub(super) async fn new(
        stolen_stream: TcpStream,
        original_destination: SocketAddr,
        connection_id: ConnectionId,
        filters: Arc<DashMap<ClientId, Regex>>,
        matched_tx: Sender<HttpHandlerRequest>,
        unmatched_tx: UnmatchedSender,
        connection_close_sender: Sender<ConnectionId>,
    ) -> Result<Self, HttpTrafficError> {
        let reversible_stream = DefaultReversibleStream::read_header(stolen_stream).await;

        match reversible_stream.map(|mut stream| {
            let http_version =
                HttpVersion::new(stream.get_header(), &H2_PREFACE[..MINIMAL_HEADER_SIZE]);

            (stream, http_version)
        }) {
            Ok((reversible_stream, http_version)) => Ok(Self {
                http_version,
                client_filters: filters,
                reversible_stream,
                original_destination,
                connection_id,
                matched_tx,
                unmatched_tx,
                connection_close_sender,
            }),
            Err(fail) => {
                error!("Something went wrong in http filter {fail:#?}");
                Err(fail)
            }
        }
    }

    /// Creates the hyper task, and returns an [`HttpFilter`] that contains the channels we use to
    /// pass the requests to the layer.
    #[tracing::instrument(
        level = "debug",
        skip(self),
        fields(
            self.http_version = ?self.http_version,
            self.client_filters = ?self.client_filters,
            self.connection_id = ?self.connection_id,
            self.original_destination = ?self.original_destination,
        )
    )]
    pub(super) fn start(self) -> Result<(), HttpTrafficError> {
        let Self {
            http_version,
            client_filters,
            matched_tx,
            unmatched_tx,
            mut reversible_stream,
            connection_id,
            original_destination,
            connection_close_sender,
        } = self;

        let port = original_destination.port();
        let connection_close_sender = self.connection_close_sender.clone();

        match http_version {
            HttpVersion::V1 => {
                tokio::task::spawn(async move {
                    // TODO: do we need to do something with this result?
                    let _res = http1::Builder::new()
                        .serve_connection(
                            reversible_stream,
                            HyperHandler {
                                filters: client_filters,
                                matched_tx,
                                unmatched_tx,
                                connection_id,
                                port,
                                original_destination,
                                request_id: 0,
                            },
                        )
                        .await;
                    connection_close_sender.send(connection_id);
                });
                Ok(())
            }
            // TODO(alex): hyper handling of HTTP/2 requires a bit more work, as it takes an
            // "executor" (just `tokio::spawn` in the `Builder::new` function is good enough), and
            // some more effort to chase some missing implementations.
            HttpVersion::V2 | HttpVersion::NotHttp => {
                let _passhtrough_task = tokio::task::spawn(async move {
                    let mut interceptor_to_original =
                        TcpStream::connect(original_destination).await?;

                    copy_bidirectional(&mut reversible_stream, &mut interceptor_to_original)
                        .await?;
                    Ok::<_, error::HttpTrafficError>(())
                });

                Ok(())
            }
        }
    }
}
