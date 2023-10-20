//! [`BackgroundTask`] used by [`Incoming`](super::IncomingProxy) to manage a single
//! intercepted raw connection.

use std::{
    io::{self, ErrorKind},
    net::SocketAddr,
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpSocket,
    time,
};

use super::InterceptorMessageOut;
use crate::background_tasks::{BackgroundTask, MessageBus};

/// Manages a single intercepted raw connection.
/// Multiple instances are run as [`BackgroundTask`]s by one [`IncomingProxy`](super::IncomingProxy)
/// to manage individual connections.
pub struct RawInterceptor {
    /// Local layer listener.
    local_destination: SocketAddr,
    /// Socket to make a connection from.
    /// This socket is already bound to an address.
    socket: TcpSocket,
}

impl RawInterceptor {
    /// Creates a new instance. This instance will connect to the provided `local_destination` using
    /// the given `socket`.
    pub fn new(local_destination: SocketAddr, socket: TcpSocket) -> Self {
        Self {
            local_destination,
            socket,
        }
    }
}

impl BackgroundTask for RawInterceptor {
    type Error = io::Error;
    type MessageIn = Vec<u8>;
    type MessageOut = InterceptorMessageOut;

    async fn run(self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let stream = self.socket.connect(self.local_destination).await?;
        let (mut read_half, mut write_half) = stream.into_split();

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
