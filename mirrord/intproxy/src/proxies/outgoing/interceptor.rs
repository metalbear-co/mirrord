use std::{fmt, io};

use mirrord_protocol::ConnectionId;
use thiserror::Error;
use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    task::JoinHandle,
};

use super::PreparedSocket;
use crate::{error::BackgroundTaskDown, protocol::NetProtocol};

#[derive(Error, Debug)]
pub enum InterceptorError {
    #[error("background task panicked")]
    Panic,
    #[error("{0}")]
    Io(io::Error),
}

struct BackgroundTask {
    tx: Sender<Vec<u8>>,
    rx: Receiver<Vec<u8>>,
    socket: PreparedSocket,
}

impl BackgroundTask {
    async fn run(mut self) -> io::Result<()> {
        let mut connected_socket = self.socket.accept().await?;

        loop {
            tokio::select! {
                biased; // To allow local socket to be read before being closed

                read = connected_socket.receive() => match read {
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        continue;
                    },
                    Err(e) => break Err(e),
                    Ok(bytes) if bytes.len() == 0 => break Ok(()),
                    Ok(bytes) => {
                        if self.tx.send(bytes).await.is_err() {
                            break Ok(());
                        }
                    },
                },

                bytes = self.rx.recv() => match bytes {
                    Some(bytes) => connected_socket.send(&bytes).await?,
                    None => break Ok(()),
                },
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct InterceptorId {
    pub connection_id: ConnectionId,
    pub protocol: NetProtocol,
}

impl fmt::Display for InterceptorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "outgoing interceptor {}-{}",
            self.connection_id, self.protocol
        )
    }
}

pub struct Interceptor {
    id: InterceptorId,
    task: JoinHandle<io::Result<()>>,
    tx: Sender<Vec<u8>>,
}

impl Interceptor {
    const CHANNEL_SIZE: usize = 512;

    pub fn new(
        connection_id: ConnectionId,
        protocol: NetProtocol,
        socket: PreparedSocket,
        tx: Sender<Vec<u8>>,
    ) -> Self {
        let (to_layer_tx, to_layer_rx) = mpsc::channel(Self::CHANNEL_SIZE);

        let task = BackgroundTask {
            tx,
            rx: to_layer_rx,
            socket,
        };

        let task = tokio::spawn(task.run());

        let interceptor = Self {
            id: InterceptorId {
                connection_id,
                protocol,
            },
            task,
            tx: to_layer_tx,
        };

        interceptor
    }

    pub async fn send_bytes(&self, bytes: Vec<u8>) -> Result<(), BackgroundTaskDown> {
        self.tx.send(bytes).await.map_err(|_| BackgroundTaskDown)
    }

    pub async fn result(self) -> Result<(), InterceptorError> {
        self.task
            .await
            .map_err(|_| InterceptorError::Panic)?
            .map_err(InterceptorError::Io)
    }
}
