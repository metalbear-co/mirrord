use std::{error::Report, fmt};

use tokio::{runtime::Handle, sync::mpsc, task::JoinHandle};

use super::{tcp_task, ConnectionInfo, IncomingIO, IncomingStream};

/// A redirected TCP connection.
///
/// No data is received nor sent via the connection until the connection task
/// is started with either [`Self::steal`] or [`Self::pass_through`].
pub struct RedirectedTcp {
    io: Box<dyn IncomingIO>,
    info: ConnectionInfo,
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
        todo!()
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
            stream: IncomingStream {
                rx: Some(incoming_rx),
            },
            data_tx: outgoing_tx,
        };

        let destination = tcp_task::Destination::StealingClient {
            data_tx: incoming_tx,
            data_rx: outgoing_rx,
        };
        let task = tcp_task::TcpTask {
            incoming_io: self.io,
            destination,
        };
        self.runtime_handle.spawn(task.run());

        stolen
    }

    /// Starts the connection task in the background.
    ///
    /// All data will be directed to the original destination.
    pub fn pass_through(self) -> JoinHandle<()> {
        self.runtime_handle.spawn(async move {
            let destination = match tcp_task::Destination::pass_through(&self.info).await {
                Ok(destination) => destination,
                Err(error) => {
                    tracing::warn!(
                        error = %Report::new(&error),
                        info = ?self.info,
                        "Failed to make a passthrough TCP connection to the original destination",
                    );

                    return;
                }
            };

            let task = tcp_task::TcpTask {
                incoming_io: self.io,
                destination,
            };

            task.run().await;
        })
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
    pub data_tx: mpsc::Sender<Vec<u8>>,
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
