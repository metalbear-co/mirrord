use std::{net::SocketAddr, sync::Arc, time::Duration};

use dashmap::DashMap;
use fancy_regex::Regex;
use hyper::server::conn::http1;
use mirrord_protocol::ConnectionId;
use tokio::{io::copy_bidirectional, net::TcpStream, sync::mpsc::Sender};
use tracing::{error, trace};

use super::{hyper_handler::HyperHandler, DefaultReversibleStream, HttpVersion};
use crate::{steal::HandlerHttpRequest, util::ClientId};

const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0";
const DEFAULT_HTTP_VERSION_DETECTION_TIMEOUT: Duration = Duration::from_secs(10);

/// Controls the amount of data we read when trying to detect if the stream's first message contains
/// an HTTP request.
///
/// **WARNING**: Can't be too small, otherwise we end up accepting things like "Foo " as valid HTTP
/// requests.
pub(super) const MINIMAL_HEADER_SIZE: usize = 10;

/// Read the start of the TCP stream, decide if it's HTTP (of a supported version), if it is, serve
/// the connection with a [`HyperHandler`]. If it isn't, just forward the whole TCP connection to
/// the original destination.
#[tracing::instrument(
    level = "trace",
    skip(stolen_stream, matched_tx, connection_close_sender)
)]
pub(super) async fn filter_task(
    stolen_stream: TcpStream,
    original_destination: SocketAddr,
    connection_id: ConnectionId,
    filters: Arc<DashMap<ClientId, Regex>>,
    matched_tx: Sender<HandlerHttpRequest>,
    connection_close_sender: Sender<ConnectionId>,
) {
    let port = original_destination.port();
    match DefaultReversibleStream::read_header(
        stolen_stream,
        DEFAULT_HTTP_VERSION_DETECTION_TIMEOUT,
    )
    .await
    {
        Ok(mut reversible_stream) => {
            match HttpVersion::new(
                reversible_stream.get_header(),
                &H2_PREFACE[..MINIMAL_HEADER_SIZE],
            ) {
                HttpVersion::V1 => {
                    // TODO: do we need to do something with this result?
                    let _res = http1::Builder::new()
                        .preserve_header_case(true)
                        .serve_connection(
                            reversible_stream,
                            HyperHandler {
                                filters,
                                matched_tx,
                                connection_id,
                                port,
                                original_destination,
                                request_id: 0,
                            },
                        )
                        .with_upgrades()
                        .await;

                    let _res = connection_close_sender
                        .send(connection_id)
                        .await
                        .inspect_err(|connection_id| {
                            error!("Main TcpConnectionStealer dropped connection close channel while HTTP filter is still running. \
                            Cannot report the closing of connection {connection_id}.");
                        });
                }

                // TODO(alex): hyper handling of HTTP/2 requires a bit more work, as it takes an
                // "executor" (just `tokio::spawn` in the `Builder::new` function is good enough),
                // and some more effort to chase some missing implementations.
                HttpVersion::V2 | HttpVersion::NotHttp => {
                    trace!(
                        "Got a connection with unsupported protocol version, passing it through \
                        to its original destination."
                    );
                    match TcpStream::connect(original_destination).await {
                        Ok(mut interceptor_to_original) => {
                            match copy_bidirectional(
                                &mut reversible_stream,
                                &mut interceptor_to_original,
                            )
                            .await
                            {
                                Ok((incoming, outgoing)) => {
                                    trace!(
                                        "Forwarded {incoming} incoming bytes and {outgoing} \
                                        outgoing bytes in passthrough connection"
                                    )
                                }
                                Err(err) => {
                                    error!(
                                        "Encountered error while forwarding unsupported \
                                    connection to its original destination: {err:?}"
                                    )
                                }
                            };
                        }
                        Err(err) => {
                            error!(
                                "Could not connect to original destination {original_destination:?}\
                                 . Received a connection with an unsupported protocol version to a \
                                 filtered HTTP port, but cannot forward the connection because of \
                                 the connection error: {err:?}");
                        }
                    }
                }
            }
        }

        Err(read_error) => {
            error!("Got error while trying to read first bytes of TCP stream: {read_error:?}");
        }
    }
}
