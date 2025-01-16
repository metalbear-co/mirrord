use std::{io::ErrorKind, net::SocketAddr, time::Duration};

use bytes::BytesMut;
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time,
};

use super::{
    bound_socket::BoundTcpSocket,
    tasks::{InProxyTaskError, InProxyTaskMessage},
};
use crate::background_tasks::{BackgroundTask, MessageBus};

pub enum LocalTcpConnection {
    FromTheStart {
        socket: BoundTcpSocket,
        peer: SocketAddr,
    },
    AfterUpgrade(OnUpgrade),
}

pub struct TcpProxyTask {
    connection: LocalTcpConnection,
    discard_data: bool,
}

impl TcpProxyTask {
    pub fn new(connection: LocalTcpConnection, discard_data: bool) -> Self {
        Self {
            connection,
            discard_data,
        }
    }
}

impl BackgroundTask for TcpProxyTask {
    type Error = InProxyTaskError;
    type MessageIn = Vec<u8>;
    type MessageOut = InProxyTaskMessage;

    async fn run(self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let mut stream = match self.connection {
            LocalTcpConnection::FromTheStart { socket, peer } => {
                let Some(stream) = message_bus
                    .closed()
                    .cancel_on_close(socket.connect(peer))
                    .await
                else {
                    return Ok(());
                };

                stream?
            }

            LocalTcpConnection::AfterUpgrade(on_upgrade) => {
                let upgraded = on_upgrade.await.map_err(InProxyTaskError::UpgradeError)?;
                let parts = upgraded
                    .downcast::<TokioIo<TcpStream>>()
                    .expect("IO type is known");
                let stream = parts.io.into_inner();
                let read_buf = parts.read_buf;

                if !self.discard_data {
                    message_bus.send(Vec::from(read_buf)).await;
                }

                stream
            }
        };

        let mut buf = BytesMut::with_capacity(64 * 1024);
        let mut reading_closed = false;
        let mut is_lingering = false;

        loop {
            tokio::select! {
                res = stream.read_buf(&mut buf), if !reading_closed => match res {
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {},
                    Err(e) => break Err(e.into()),
                    Ok(..) => {
                        if buf.is_empty() {
                            reading_closed = true;
                        }

                        if !self.discard_data {
                            message_bus.send(buf.to_vec()).await;
                        }

                        buf.clear();
                    }
                },

                msg = message_bus.recv(), if !is_lingering => match msg {
                    None => {
                        if self.discard_data {
                            break Ok(());
                        }

                        is_lingering = true;
                    }
                    Some(data) => {
                        if data.is_empty() {
                            stream.shutdown().await?;
                        } else {
                            stream.write_all(&data).await?;
                        }
                    },
                },

                _ = time::sleep(Duration::from_secs(1)), if is_lingering => {
                    break Ok(());
                }
            }
        }
    }
}
