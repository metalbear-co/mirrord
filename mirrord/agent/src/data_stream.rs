//! Routing of QUIC data streams to the feature that owns the connection each one carries.
//!
//! When a client is connected over QUIC and both ends negotiated
//! [`DATA_STREAM_VERSION`](mirrord_quic::DATA_STREAM_VERSION), the bytes of an intercepted
//! connection travel on their own stream instead of being framed into mirrord-protocol messages.
//! Everything else about the session still goes over the control stream.
//!
//! Streams are opened by the operator once the control stream has told it a connection exists, so
//! by the time one arrives here, the feature that owns the connection is already holding the socket
//! and waiting for it.

use mirrord_protocol::ConnectionId;
use mirrord_quic::{BiStream, DataStreamKind};
use tokio::sync::mpsc::Sender;
use tracing::Level;

/// A data stream, handed to the feature that owns the connection it carries.
pub(crate) struct IncomingDataStream {
    pub(crate) connection_id: ConnectionId,
    pub(crate) stream: BiStream,
}

/// Accepts data streams for as long as the client's QUIC connection lives, handing each one to the
/// feature named by its header.
///
/// Returns when the connection is gone, or when the feature that would own the streams has stopped
/// listening for them.
#[tracing::instrument(level = Level::TRACE, skip_all)]
pub(crate) async fn route_data_streams(
    connection: quinn::Connection,
    tcp_outgoing: Sender<IncomingDataStream>,
) {
    loop {
        let (header, stream) = match mirrord_quic::accept_data_stream(&connection).await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::trace!(%error, "Stopped accepting data streams");
                break;
            }
        };

        let sent = match header.kind {
            DataStreamKind::TcpOutgoing => {
                tcp_outgoing
                    .send(IncomingDataStream {
                        connection_id: header.connection_id,
                        stream,
                    })
                    .await
            }
        };

        if sent.is_err() {
            tracing::trace!("Data stream consumer is gone, stopped accepting data streams");
            break;
        }
    }
}
