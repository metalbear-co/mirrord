//! [`BackgroundTask`] used by [`Incoming`](super::IncomingProxy) to manage a single
//! intercepted raw connection.

use std::{
    io::{self, ErrorKind},
    net::SocketAddr,
    time::Duration,
};

use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpStream,
    },
    time,
};

use super::InterceptorMessageOut;
use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    codec::{AsyncEncoder, CodecError},
};

/// Errors that can occur when executing [`RawInterceptor`] as a [`BackgroundTask`].
#[derive(Error, Debug)]
pub enum RawInterceptorError {
    /// IO failed.
    #[error("io failed: {0}")]
    IoError(#[from] io::Error),
    /// [`codec`](crate::codec) failed.
    #[error("{0}")]
    CodecError(#[from] CodecError),
}

/// Manages a single intercepted raw connection.
/// Multiple instances are run as [`BackgroundTask`]s by one [`IncomingProxy`](super::IncomingProxy)
/// to manage individual connections.
pub struct RawInterceptor {
    /// Original source of data provided by the agent.
    remote_source: SocketAddr,
    /// Local layer listener.
    local_destination: SocketAddr,
}

impl RawInterceptor {
    /// Creates a new instance. This instance will connect to the provided `local_destination`.
    pub fn new(remote_source: SocketAddr, local_destination: SocketAddr) -> Self {
        Self {
            remote_source,
            local_destination,
        }
    }

    /// Connects to the local listener and sends it encoded address of the remote peer.
    async fn connect_and_send_source(
        &self,
    ) -> Result<(OwnedReadHalf, OwnedWriteHalf), RawInterceptorError> {
        let stream = TcpStream::connect(self.local_destination).await?;

        let mut codec_tx: AsyncEncoder<SocketAddr, TcpStream> = AsyncEncoder::new(stream);
        codec_tx.send(&self.remote_source).await?;
        codec_tx.flush().await?;

        let stream = codec_tx.into_inner();

        Ok(stream.into_split())
    }
}

impl BackgroundTask for RawInterceptor {
    type Error = RawInterceptorError;
    type MessageIn = Vec<u8>;
    type MessageOut = InterceptorMessageOut;

    async fn run(self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let (mut read_half, mut write_half) = self.connect_and_send_source().await?;

        let mut buffer = vec![0; 1024];
        let mut remote_closed = false;

        loop {
            tokio::select! {
                biased;

                res = read_half.read(&mut buffer[..]) => match res {
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {},
                    Err(e) => break Err(e.into()),
                    Ok(0) => break Ok(()),
                    Ok(read) => {
                        message_bus.send(InterceptorMessageOut::Bytes(buffer.get(..read).unwrap().to_vec())).await;
                    }
                },

                msg = message_bus.recv(), if !remote_closed => match msg {
                    None => {
                        tracing::trace!("message bus closed, waiting 1 second before exiting");
                        remote_closed = true;
                    },
                    Some(data) => {
                        write_half.write_all(&data).await?;
                    }
                },

                _ = time::sleep(Duration::from_secs(1)), if remote_closed => {
                    tracing::trace!("layer silent for 1 second and message bus is closed, exiting");
                    break Ok(());
                },
            }
        }
    }
}
