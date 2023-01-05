use std::{net::SocketAddr, sync::Arc};

use dashmap::DashMap;
use fancy_regex::Regex;
use hyper::server::conn::http1;
use mirrord_protocol::ConnectionId;
use tokio::{io::copy_bidirectional, net::TcpStream, sync::mpsc::Sender};
use tracing::error;

use super::{
    error::HttpTrafficError, hyper_handler::HyperHandler, DefaultReversibleStream, HttpVersion,
};
use crate::{
    steal::{http_traffic::error, HandlerHttpRequest},
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

    /// For informing the stealer task that the connection was closed.
    connection_close_sender: Sender<ConnectionId>,
}

impl HttpFilterBuilder {
    /// Does not consume bytes from the stream.
    ///
    /// Checks if the first available bytes in a stream could be of an http request.
    ///
    /// This is a best effort classification, not a guarantee that the stream is HTTP.
    #[tracing::instrument(
        level = "trace",
        skip_all
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
        matched_tx: Sender<HandlerHttpRequest>,
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
        level = "trace",
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
            mut reversible_stream,
            connection_id,
            original_destination,
            connection_close_sender,
        } = self;

        let port = original_destination.port();

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
                                connection_id,
                                port,
                                original_destination,
                                request_id: 0,
                            },
                        )
                        .await;

                    let _res = connection_close_sender
                        .send(connection_id)
                        .await
                        .inspect_err(|connection_id| {
                            error!("Main TcpConnectionStealer dropped connection close channel while HTTP filter is still running. \
                            Cannot report the closing of connection {connection_id}.");
                        });
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
