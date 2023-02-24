use std::{future::Future, net::SocketAddr, sync::Arc, time::Duration};

use dashmap::DashMap;
use fancy_regex::Regex;
use hyper::{
    rt::Executor,
    server::{
        self,
        conn::{http1, http2},
    },
};
use mirrord_protocol::ConnectionId;
use tokio::{
    io::{copy_bidirectional, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc::Sender, oneshot},
};
use tracing::{error, trace};

use super::{
    error::HttpTrafficError,
    hyper_handler::{httpv1::HttpV1, httpv2::HttpV2, HyperHandler, RawHyperConnection},
    DefaultReversibleStream, HttpVersion,
};
use crate::{steal::HandlerHttpRequest, util::ClientId};

const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0";
const DEFAULT_HTTP_VERSION_DETECTION_TIMEOUT: Duration = Duration::from_secs(10);

// TODO(alex): Import this from `hyper-util` when the crate is actually published.
/// Future executor that utilises `tokio` threads.
#[non_exhaustive]
#[derive(Default, Debug, Clone)]
pub struct TokioExecutor;

impl<Fut> Executor<Fut> for TokioExecutor
where
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
{
    fn execute(&self, fut: Fut) {
        trace!("starting tokio executor for hyper HTTP/2");
        tokio::spawn(fut);
    }
}

/// Controls the amount of data we read when trying to detect if the stream's first message contains
/// an HTTP request.
///
/// **WARNING**: Can't be too small, otherwise we end up accepting things like "Foo " as valid HTTP
/// requests.
pub(super) const MINIMAL_HEADER_SIZE: usize = 10;

/// Reads the start of the [`TcpStream`], and decides if it's HTTP (we currently only support
/// HTTP/1) or not,
///
/// ## HTTP/1
///
/// If the stream is identified as HTTP/1 by our check in [`HttpVersion::new`], then we serve the
/// connection with [`HyperHandler`].
///
/// ### Upgrade
///
/// If an upgrade request is detected in the [`HyperHandler`], then we take the HTTP connection
/// that's being served (after HTTP processing is done), and use [`copy_bidirectional`] to copy the
/// data from the upgraded connection to its original destination (similar to the Not HTTP/1
/// handling).
///
/// ## Not HTTP/1
///
/// Forwards the whole TCP connection to the original destination with [`copy_bidirectional`].
///
/// It's important to note that, we don't lose the bytes read from the original stream, due to us
/// converting it into a  [`DefaultReversibleStream`].
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
                            reversible_stream,
                            HyperHandler::<HttpV1>::new(
                                filters,
                                matched_tx,
                                connection_id,
                                port,
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
                    // We have to keep the connection alive to handle a possible upgrade request
                    // manually.
                    http2::Builder::new(TokioExecutor::default())
                        .serve_connection(
                            reversible_stream,
                            HyperHandler::<HttpV2>::new(
                                filters,
                                matched_tx,
                                connection_id,
                                port,
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

#[tracing::instrument(level = "trace", skip(sender))]
async fn close_connection(
    sender: Sender<ConnectionId>,
    connection_id: ConnectionId,
) -> Result<(), HttpTrafficError> {
    sender
        .send(connection_id)
        .await
        .inspect_err(|connection_id| {
            error!(r"
                Main TcpConnectionStealer dropped connection close channel while HTTP filter is still running.
                Cannot report the closing of connection {connection_id}."
            ); 
        })
        .map_err(From::from)
}
