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
    PassthroughRequest,
};
use crate::{
    steal::{http_traffic::error, StealerHttpRequest},
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
    original_address: SocketAddr,
    connection_id: ConnectionId,
    client_filters: Arc<DashMap<ClientId, Regex>>,
    captured_tx: Sender<StealerHttpRequest>,
    passthrough_tx: Sender<PassthroughRequest>,
}

/// Used by the stealer handler to:
///
/// 1. Read the requests from hyper's channels through [`captured_rx`], and [`passthrough_rx`];
/// 2. Send the raw bytes we got from the remote connection to hyper through [`interceptor_stream`];
pub(crate) struct HttpFilter {
    pub(super) hyper_task: JoinHandle<Result<(), HttpTrafficError>>,
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
    #[tracing::instrument(level = "debug")]
    pub(super) async fn new(
        stolen_stream: TcpStream,
        original_address: SocketAddr,
        connection_id: ConnectionId,
        filters: Arc<DashMap<ClientId, Regex>>,
        captured_tx: Sender<StealerHttpRequest>,
        passthrough_tx: Sender<PassthroughRequest>,
    ) -> Result<Self, HttpTrafficError> {
        let reversible_stream = DefaultReversibleStream::read_header(stolen_stream).await;

        match reversible_stream.and_then(|mut stream| {
            let http_version =
                HttpVersion::new(stream.get_header(), &H2_PREFACE[..MINIMAL_HEADER_SIZE]);

            Ok((stream, http_version))
        }) {
            Ok((reversible_stream, http_version)) => Ok(Self {
                http_version,
                client_filters: filters,
                reversible_stream,
                original_address,
                connection_id,
                captured_tx,
                passthrough_tx,
            }),
            Err(fail) => {
                error!("Something went wrong in http filter {fail:#?}");
                Err(fail)
            }
        }
    }

    /// Creates the hyper task, and returns an [`HttpFilter`] that contains the channels we use to
    /// pass the requests to the layer.
    #[tracing::instrument(level = "debug")]
    pub(super) fn start(self) -> Result<Option<HttpFilter>, HttpTrafficError> {
        let Self {
            http_version,
            client_filters,
            captured_tx,
            passthrough_tx,
            mut reversible_stream,
            original_address,
            connection_id,
        } = self;

        // Incoming tcp stream definitely has an address.
        let port = reversible_stream.local_addr().unwrap().port();

        match http_version {
            HttpVersion::V1 => {
                let (hyper_stream, interceptor_stream) = duplex(15000);

                let hyper_task = tokio::task::spawn(async move {
                    http1::Builder::new()
                        .serve_connection(
                            hyper_stream,
                            HyperHandler {
                                filters: client_filters,
                                captured_tx,
                                passthrough_tx,
                                connection_id,
                                port,
                                request_id: 0,
                            },
                        )
                        .await
                        .map_err(From::from)
                });

                Ok(Some(HttpFilter {
                    hyper_task,
                    reversible_stream,
                    interceptor_stream,
                }))
            }
            // TODO(alex): hyper handling of HTTP/2 requires a bit more work, as it takes an
            // "executor" (just `tokio::spawn` in the `Builder::new` function is good enough), and
            // some more effort to chase some missing implementations.
            HttpVersion::V2 | HttpVersion::NotHttp => {
                let passhtrough_task = tokio::task::spawn(async move {
                    let mut interceptor_to_original = TcpStream::connect(original_address).await?;

                    copy_bidirectional(&mut reversible_stream, &mut interceptor_to_original)
                        .await?;
                    Ok::<_, error::HttpTrafficError>(())
                });

                Ok(None)
            }
        }
    }
}
