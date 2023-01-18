use std::{net::SocketAddr, sync::Arc, time::Duration};

use dashmap::DashMap;
use fancy_regex::Regex;
use hyper::server::{self, conn::http1};
use mirrord_protocol::ConnectionId;
use tokio::{
    io::copy_bidirectional,
    net::TcpStream,
    select,
    sync::{mpsc::Sender, oneshot},
};
use tracing::{error, info, trace};

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
// #[tracing::instrument(
//     level = "trace",
//     skip(stolen_stream, matched_tx, connection_close_sender)
// )]
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

                    // TODO(alex) [high] 2023-01-18: Hnald the upgrade here!
                    // From hyper_handler, we just send the upgrade request to the original
                    // destination, then receive and pass the upgrade response to the browser.
                    //
                    // We should get here when the response is done, so now we have access to the
                    // stream (agent-browser), but we also need access to the (agent-original)
                    // connection, that is being held in the inner unmatched task.
                    //
                    // We could add an `upgrade` channel to `HyperHandler`, and check for it here,
                    // something that sends `Some(connection)` if `upgrade == true` in the task,
                    // allowing us to await with no issues here.
                    //
                    // Should also probably drop the `with_upgrades` thing, otherwise hyper may
                    // think that the service is not done, and block us from doing this.
                    let (upgrade_tx, upgrade_rx) = oneshot::channel::<Option<TcpStream>>();
                    let hyper_result = http1::Builder::new()
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
                                upgrade_tx: Some(upgrade_tx),
                            },
                        )
                        .without_shutdown()
                        .await;

                    {}

                    select! {
                        interceptor_to_original = upgrade_rx => {
                            info!("Have interceptor to original stream in {:#?}", interceptor_to_original);
                            info!("We also have hyper raw {:#?}", hyper_result);

                            let server::conn::http1::Parts {
                                io: mut remote_to_agent,
                                read_buf,
                                service,
                                ..
                            } = hyper_result.unwrap();

                            match interceptor_to_original {
                                Ok(Some(mut interceptor_to_original)) => {
                                    copy_bidirectional(&mut interceptor_to_original, &mut remote_to_agent).await.unwrap();
                                },
                                Err(fail) => {
                                    error!("Failed retrieving from the interceptor to original channel with {fail:#?}");
                                }
                                _ => { info!("No data in interceptor to original channel, not upgrade!"); }
                            }
                        }
                    };

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
