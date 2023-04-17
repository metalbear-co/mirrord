//! Contains a couple of handy constants and the [`filter_task`].
//!
//! # [`filter_task`]
//!
//! The _meaty_ part of our HTTP traffic stealing feature, that creates the whole HTTP filtering
//! mechanism.
//!
//! # [`TokioExecutor`]
//!
//! Temporary affair required for HTTP/2 handshaking in [`hyper`].
//!
//! # [`close_connection`]
//!
//! Notifies that the connection for this current [`filter_task`] should be closed.
use std::{future::Future, net::SocketAddr, sync::Arc, time::Duration};

use dashmap::DashMap;
use fancy_regex::Regex;
use hyper::rt::Executor;
use mirrord_protocol::ConnectionId;
use tokio::{io::copy_bidirectional, net::TcpStream, sync::mpsc::Sender};
use tracing::{error, trace};

use super::{
    error::HttpTrafficError, v1::HttpV1, v2::HttpV2, DefaultReversibleStream, HttpV, HttpVersion,
};
use crate::{steal::HandlerHttpRequest, util::ClientId};

/// Default start of an HTTP/2 request.
///
/// Used by [`HttpVersion`] to check if the connection should be treated as HTTP/2.
const H2_PREFACE: &[u8] = b"PRI * HTTP/2.0";

/// Timeout value for how long we wait for a stream to contain enough bytes to assert which HTTP
/// version we're dealing with.
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
/// ## HTTP/1 and HTTP/2
///
/// If the stream is identified as HTTP/1 by our check in [`HttpVersion::new`], then we serve the
/// connection with [`HyperHandler`].
///
/// ### Upgrade (HTTP/1 only)
///
/// If an upgrade request is detected in the [`HyperHandler`], then we take the HTTP connection
/// that's being served (after HTTP processing is done), and use [`copy_bidirectional`] to copy the
/// data from the upgraded connection to its original destination (similar to the "Not HTTP"
/// handling).
///
/// ## Not HTTP
///
/// Forwards the whole TCP connection to the original destination with [`copy_bidirectional`].
///
/// It's important to note that, we don't lose the bytes read from the original stream, due to us
/// converting it into a [`DefaultReversibleStream`].
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
                    HttpV1::serve_connection(
                        reversible_stream,
                        original_destination,
                        connection_id,
                        filters,
                        matched_tx,
                        connection_close_sender,
                    )
                    .await
                }
                HttpVersion::V2 => {
                    HttpV2::serve_connection(
                        reversible_stream,
                        original_destination,
                        connection_id,
                        filters,
                        matched_tx,
                        connection_close_sender,
                    )
                    .await
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
                                 the connection error: {err:?}"
                        );
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

/// Notifies the [`filter_task`] that the connection with `connection_id` should be closed.
#[tracing::instrument(level = "trace", skip(sender))]
pub(super) async fn close_connection(
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
