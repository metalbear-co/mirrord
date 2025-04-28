use tokio::{
    runtime::Handle,
    sync::{broadcast, mpsc},
};
use tokio_stream::wrappers::BroadcastStream;

use super::{ConnectionInfo, IncomingIO, IncomingStream, IncomingStreamItem};

mod task;

/// A redirected TCP connection.
///
/// No data is received nor sent via the connection until the connection task
/// is started with either [`Self::steal`] or [`Self::pass_through`].
pub struct RedirectedTcp {
    io: Box<dyn IncomingIO>,
    info: ConnectionInfo,
    /// [`None`] until [`Self::mirror`] is called.
    copy_tx: Option<broadcast::Sender<IncomingStreamItem>>,
    /// Handle to the [`tokio::runtime`] in which this struct was created.
    ///
    /// Used to spawn the connection task.
    ///
    /// Thanks to this handle, this struct can be freely moved across runtimes.
    runtime_handle: Handle,
}

impl RedirectedTcp {
    /// Should be called in the target's Linux network namespace.
    pub fn new(io: Box<dyn IncomingIO>, info: ConnectionInfo) -> Self {
        Self {
            io,
            info,
            copy_tx: None,
            runtime_handle: Handle::current(),
        }
    }

    pub fn info(&self) -> &ConnectionInfo {
        &self.info
    }

    /// Acquires a mirror handle to this connection.
    pub fn mirror(&mut self) -> MirroredTcp {
        let rx = self
            .copy_tx
            .as_ref()
            .map(|tx| tx.subscribe())
            .unwrap_or_else(|| {
                let (tx, rx) = broadcast::channel(32);
                self.copy_tx.replace(tx);
                rx
            });

        MirroredTcp {
            info: self.info.clone(),
            stream: IncomingStream::Broadcast(BroadcastStream::new(rx)),
        }
    }

    /// Acquires a steal handle to this connection,
    /// and starts the connection task in the background.
    ///
    /// All data will be directed to this handle.
    pub fn steal(self) -> StolenTcp {
        let (incoming_tx, incoming_rx) = mpsc::channel(32);
        let (outgoing_tx, outgoing_rx) = mpsc::channel(32);

        let stolen = StolenTcp {
            info: self.info.clone(),
            stream: IncomingStream::Mpsc(incoming_rx),
            data_tx: outgoing_tx,
        };

        let destination = task::Destination::StealingClient {
            data_tx: incoming_tx.into(),
            data_rx: outgoing_rx,
        };
        let task = task::TcpTask {
            incoming_io: self.io,
            destination,
            copy_tx: self.copy_tx.into(),
        };
        self.runtime_handle.spawn(task.run());

        stolen
    }

    /// Starts the connection task in the background.
    ///
    /// All data will be directed to the original destination.
    pub fn pass_through(self) {
        self.runtime_handle.spawn(async move {
            let destination = match task::Destination::pass_through(&self.info).await {
                Ok(destination) => destination,
                Err(error) => {
                    if let Some(tx) = self.copy_tx {
                        let _ = tx.send(IncomingStreamItem::Finished(Err(error)));
                    }

                    return;
                }
            };

            let task = task::TcpTask {
                incoming_io: self.io,
                destination,
                copy_tx: self.copy_tx.into(),
            };

            task.run().await;
        });
    }
}

/// Mirror handle to a redirected connection.
pub struct MirroredTcp {
    pub info: ConnectionInfo,
    pub stream: IncomingStream,
}

/// Steal handle to a redirected connection.
pub struct StolenTcp {
    pub info: ConnectionInfo,
    pub stream: IncomingStream,
    /// Can be used to send data to the peer.
    ///
    /// Dropping this sender will be interpreted as a write shutdown,
    /// and will not terminate the connection right away.
    pub data_tx: mpsc::Sender<Vec<u8>>,
}
