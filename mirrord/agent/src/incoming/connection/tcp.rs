use std::{error::Report, fmt};

use bytes::{Bytes, BytesMut};
use mirrord_tls_util::MaybeTls;
use tokio::{
    io::AsyncWriteExt,
    net::TcpStream,
    runtime::Handle,
    sync::{broadcast, mpsc},
    task::JoinHandle,
};
use tokio_stream::wrappers::BroadcastStream;
use tokio_util::sync::CancellationToken;

use super::{ConnectionInfo, IncomingIO, IncomingStream};
use crate::incoming::{
    ConnError, IncomingStreamItem,
    connection::{
        copy_bidirectional::{self, PassthroughConnection, StealingClient},
        optional_broadcast::OptionalBroadcast,
    },
};

/// A redirected TCP connection.
///
/// No data is received nor sent via the connection until the connection task
/// is started with either [`Self::steal`] or [`Self::pass_through`].
pub struct RedirectedTcp {
    io: Box<dyn IncomingIO>,
    info: ConnectionInfo,
    mirror_tx: Option<broadcast::Sender<IncomingStreamItem>>,
    /// Handle to the [`tokio::runtime`] in which this struct was created.
    ///
    /// Used to spawn the connection task.
    ///
    /// Thanks to this handle, this struct can be freely moved across runtimes.
    runtime_handle: Handle,
}

impl RedirectedTcp {
    /// Should be called in the target's Linux network namespace,
    /// as [`Handle::current()`] is stored in this struct.
    /// We might need to connect to the original destination in the future.
    pub fn new(io: Box<dyn IncomingIO>, info: ConnectionInfo) -> Self {
        Self {
            io,
            info,
            mirror_tx: None,
            runtime_handle: Handle::current(),
        }
    }

    pub fn info(&self) -> &ConnectionInfo {
        &self.info
    }

    /// Acquires a mirror handle to this connection.
    ///
    /// For the data to flow, you must start the connection task with either [`Self::steal`] or
    /// [`Self::pass_through`].
    pub fn mirror(&mut self) -> MirroredTcp {
        let rx = match &self.mirror_tx {
            Some(tx) => tx.subscribe(),
            None => {
                let (tx, rx) = broadcast::channel(32);
                self.mirror_tx = Some(tx);
                rx
            }
        };

        MirroredTcp {
            info: self.info.clone(),
            stream: IncomingStream::Mirror(BroadcastStream::new(rx)),
        }
    }

    /// Acquires a steal handle to this connection,
    /// and starts the connection task in the background.
    ///
    /// All data will be directed to this handle.
    /// The returned [`JoinHandle`] is for the spawned IO task.
    pub fn steal(mut self, shutdown: CancellationToken) -> (StolenTcp, JoinHandle<()>) {
        let (incoming_tx, incoming_rx) = mpsc::channel(32);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(32);

        let handle = self.runtime_handle.clone();
        let task = async move {
            let mut outgoing = StealingClient {
                data_tx: incoming_tx,
                data_rx: outgoing_rx,
                mirror_data_tx: self.mirror_tx.into(),
            };

            let result = tokio::select! {
                r = copy_bidirectional::copy_bidirectional(&mut self.io, &mut outgoing) => r,
                _ = shutdown.cancelled() => {
                    tracing::debug!("Gracefully shutting down stolen tcp connection");
                    if let Err(err) = self.io.shutdown().await {
                        tracing::error!(?err, "Error shutting down stolen tcp connection")
                    };

                    Err(ConnError::AgentExiting)
                }
            };

            outgoing
                .mirror_data_tx
                .send_item(IncomingStreamItem::Finished(result.clone()));
            let _ = outgoing
                .data_tx
                .send(IncomingStreamItem::Finished(result))
                .await;
        };

        let join_handle = handle.spawn(task);

        (
            StolenTcp {
                info: self.info,
                stream: IncomingStream::Steal(incoming_rx),
                data_tx: outgoing_tx,
            },
            join_handle,
        )
    }

    /// Starts the connection task in the background.
    ///
    /// All data will be directed to the original destination.
    pub fn pass_through(mut self, shutdown: CancellationToken) -> JoinHandle<()> {
        let handle = self.runtime_handle.clone();
        handle.spawn(async move {
            let mut mirror_data_tx = OptionalBroadcast::from(self.mirror_tx.take());

            let passthrough = tokio::select! {
                conn = self.make_pass_through_connection() => conn,
                _ = shutdown.cancelled() => {
                    tracing::debug!("Gracefully shutting down passed-through connection during TCP/TLS handshake");
                    mirror_data_tx
                        .send_item(IncomingStreamItem::Finished(Err(ConnError::AgentExiting)));
                    return;
                }
            };

            let stream = match passthrough {
                Ok(stream) => stream,
                Err(error) => {
                    tracing::warn!(
                        error = %Report::new(&error),
                        info = ?self.info,
                        "Failed to make a passthrough TCP connection to the original destination",
                    );
                    mirror_data_tx.send_item(IncomingStreamItem::Finished(Err(error)));
                    return;
                }
            };

            let mut outgoing = PassthroughConnection {
                stream,
                buffer: BytesMut::with_capacity(64 * 1024),
                mirror_data_tx,
            };

            let result = tokio::select! {
                r = copy_bidirectional::copy_bidirectional(&mut self.io, &mut outgoing) => r,

                _ = shutdown.cancelled() => {
                    tracing::debug!("Gracefully shutting down passed-through tcp connection");
                    if let Err(err) = self.io.shutdown().await {
                        tracing::error!(?err, "Error shutting down passed-through tcp connection")
                    };
                    Err(ConnError::AgentExiting)
                }
            };

            outgoing
                .mirror_data_tx
                .send_item(IncomingStreamItem::Finished(result));
        })
    }

    async fn make_pass_through_connection(&self) -> Result<MaybeTls, ConnError> {
        let tcp_stream = TcpStream::connect(self.info.pass_through_address())
            .await
            .map_err(From::from)
            .map_err(ConnError::TcpConnectError)?;

        match &self.info.tls_connector {
            Some(tls_connector) => {
                let stream = tls_connector
                    .connect(self.info.original_destination.ip(), None, tcp_stream)
                    .await
                    .map_err(From::from)
                    .map_err(ConnError::TlsConnectError)?;
                Ok(MaybeTls::Tls(stream))
            }
            None => Ok(MaybeTls::NoTls(tcp_stream)),
        }
    }

    #[cfg(test)]
    pub fn into_io(self) -> Box<dyn IncomingIO + 'static> {
        self.io
    }
}

impl fmt::Debug for RedirectedTcp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RedirectedTcp")
            .field("info", &self.info)
            .finish()
    }
}

/// Steal handle to a redirected connection.
pub struct StolenTcp {
    pub info: ConnectionInfo,
    /// Dropping this stream will be interpreted as dropping the connection.
    pub stream: IncomingStream,
    /// Can be used to send data to the peer.
    ///
    /// Dropping this sender will be interpreted as a write shutdown.
    pub data_tx: mpsc::Sender<Bytes>,
}

impl fmt::Debug for StolenTcp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StolenTcp")
            .field("info", &self.info)
            .finish()
    }
}

/// Mirror handle to a redirected connection.
pub struct MirroredTcp {
    pub info: ConnectionInfo,
    pub stream: IncomingStream,
}

impl fmt::Debug for MirroredTcp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MirroredTcp")
            .field("info", &self.info)
            .finish()
    }
}
