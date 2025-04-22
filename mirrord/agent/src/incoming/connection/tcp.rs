use tokio::{
    runtime::Handle,
    sync::{broadcast, mpsc},
};
use tokio_stream::wrappers::BroadcastStream;

use super::{ConnectionInfo, IncomingIO, IncomingStream, IncomingStreamItem};

mod task;

pub struct RedirectedTcp {
    io: Box<dyn IncomingIO>,
    info: ConnectionInfo,
    copy_tx: Option<broadcast::Sender<IncomingStreamItem>>,
    runtime_handle: Handle,
}

impl RedirectedTcp {
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

pub struct MirroredTcp {
    pub info: ConnectionInfo,
    pub stream: IncomingStream,
}

pub struct StolenTcp {
    pub info: ConnectionInfo,
    pub stream: IncomingStream,
    pub data_tx: mpsc::Sender<Vec<u8>>,
}
