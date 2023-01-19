use std::{net::SocketAddr, sync::Arc, time::Duration};

use dashmap::DashMap;
use fancy_regex::Regex;
use futures::{FutureExt, TryFutureExt};
use hyper::server::{self, conn::http1};
use mirrord_protocol::ConnectionId;
use tokio::{
    io::{copy_bidirectional, AsyncWriteExt},
    net::TcpStream,
    select,
    sync::{mpsc::Sender, oneshot},
};
use tracing::{error, info, trace};

use super::{
    error::HttpTrafficError,
    hyper_handler::{HyperHandler, LiveConnection},
    DefaultReversibleStream, HttpVersion,
};
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
) -> Result<(), HttpTrafficError> {
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
                    let (upgrade_tx, upgrade_rx) = oneshot::channel::<LiveConnection>();
                    let server::conn::http1::Parts {
                        io: mut client_agent, // i.e. browser-agent connection
                        read_buf: agent_unprocessed,
                        ..
                    } = http1::Builder::new()
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
                        .await?;

                    if let Ok(LiveConnection {
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

                    connection_close_sender
                        .send(connection_id)
                        .await
                        .inspect_err(|connection_id| {
                            error!("Main TcpConnectionStealer dropped connection close channel while HTTP filter is still running. \
                            Cannot report the closing of connection {connection_id}.");
                        }).map_err(From::from)
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
                                 the connection error: {err:?}");
                            Err(err)?
                        }
                    }
                }
            }
        }

        Err(read_error) => {
            error!("Got error while trying to read first bytes of TCP stream: {read_error:?}");
            Err(read_error)
        }
    }
}
