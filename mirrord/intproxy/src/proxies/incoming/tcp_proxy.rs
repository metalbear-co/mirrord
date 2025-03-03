use std::{io::ErrorKind, net::SocketAddr, time::Duration};

use bytes::BytesMut;
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use mirrord_protocol::ConnectionId;
use mirrord_tls_util::MaybeTls;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time,
};
use tracing::Level;

use super::{
    bound_socket::BoundTcpSocket,
    tasks::{InProxyTaskError, InProxyTaskMessage},
};
use crate::background_tasks::{BackgroundTask, MessageBus};

/// Local TCP connections between the [`TcpProxyTask`] and the user application.
#[derive(Debug)]
pub enum LocalTcpConnection {
    /// Not yet established. Should be made by the [`TcpProxyTask`] from the given
    /// [`BoundTcpSocket`].
    FromTheStart {
        socket: BoundTcpSocket,
        peer: SocketAddr,
    },
    /// Upgraded HTTP connection from a previously stolen HTTP request.
    AfterUpgrade(OnUpgrade),
}

/// [`BackgroundTask`] of [`IncomingProxy`](super::IncomingProxy) that handles a remote
/// stolen/mirrored TCP connection.
///
/// In steal mode, exits immediately when it's [`TaskSender`](crate::background_tasks::TaskSender)
/// is dropped.
///
/// In mirror mode, when it's [`TaskSender`](crate::background_tasks::TaskSender) is dropped,
/// this proxy keeps reading data from the user application and exits after
/// [`Self::MIRROR_MODE_LINGER_TIMEOUT`] of silence.
#[derive(Debug)]
pub struct TcpProxyTask {
    /// ID of the remote connection this task handles.
    ///
    /// Saved for [`std::fmt::Debug`] implementation.
    _connection_id: ConnectionId,
    /// The local connection between this task and the user application.
    connection: Option<LocalTcpConnection>,
    /// Whether this task should silently discard data coming from the user application.
    ///
    /// The data is discarded only when the remote connection is mirrored.
    discard_data: bool,
}

impl TcpProxyTask {
    /// Mirror mode only: how long do we wait before exiting after the [`MessageBus`] is closed
    /// and user application doesn't send any data.
    pub const MIRROR_MODE_LINGER_TIMEOUT: Duration = Duration::from_secs(1);

    /// Creates a new task.
    ///
    /// * This task will talk with the user application using the given [`LocalTcpConnection`].
    /// * If `discard_data` is set, this task will silently discard all data coming from the user
    ///   application.
    pub fn new(
        connection_id: ConnectionId,
        connection: LocalTcpConnection,
        discard_data: bool,
    ) -> Self {
        Self {
            _connection_id: connection_id,
            connection: Some(connection),
            discard_data,
        }
    }
}

impl BackgroundTask for TcpProxyTask {
    type Error = InProxyTaskError;
    type MessageIn = Vec<u8>;
    type MessageOut = InProxyTaskMessage;

    #[tracing::instrument(
        level = Level::DEBUG, name = "tcp_proxy_task_main_loop",
        skip(message_bus),
        ret, err(level = Level::WARN),
    )]
    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let mut stream = match self
            .connection
            .take()
            .expect("task should have a valid connection before run")
        {
            LocalTcpConnection::FromTheStart { socket, peer } => {
                let Some(stream) = message_bus
                    .closed()
                    .cancel_on_close(socket.connect(peer))
                    .await
                else {
                    return Ok(());
                };

                MaybeTls::NoTls(stream?)
            }

            LocalTcpConnection::AfterUpgrade(on_upgrade) => {
                let upgraded = on_upgrade.await.map_err(InProxyTaskError::UpgradeError)?;
                let parts = upgraded
                    .downcast::<TokioIo<MaybeTls>>()
                    .expect("IO type is known");
                let stream = parts.io.into_inner();
                let read_buf = parts.read_buf;

                if !self.discard_data && !read_buf.is_empty() {
                    // We don't send empty data,
                    // because the agent recognizes it as a shutdown from the user application.
                    message_bus.send(Vec::from(read_buf)).await;
                }

                stream
            }
        };

        let peer_addr = stream.as_ref().peer_addr()?;
        let self_addr = stream.as_ref().local_addr()?;

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

                            tracing::trace!(
                                peer_addr = %peer_addr,
                                self_addr = %self_addr,
                                "The user application shut down its side of the connection",
                            )
                        } else {
                            tracing::trace!(
                                data_len = buf.len(),
                                peer_addr = %peer_addr,
                                self_addr = %self_addr,
                                "Received some data from the user application",
                            );
                        }

                        if !self.discard_data {
                            message_bus.send(buf.to_vec()).await;
                        }

                        buf.clear();
                    }
                },

                msg = message_bus.recv(), if !is_lingering => match msg {
                    None if self.discard_data => {
                        tracing::trace!(
                            peer_addr = %peer_addr,
                            self_addr = %self_addr,
                            "Message bus closed, waiting until the connection is silent",
                        );

                        is_lingering = true;
                    }
                    None => {
                        tracing::trace!(
                            peer_addr = %peer_addr,
                            self_addr = %self_addr,
                            "Message bus closed, exiting",
                        );

                        break Ok(());
                    }
                    Some(data) => {
                        if data.is_empty() {
                            tracing::trace!(
                                peer_addr = %peer_addr,
                                self_addr = %self_addr,
                                "The agent shut down its side of the connection",
                            );

                            stream.shutdown().await?;
                        } else {
                            tracing::trace!(
                                data_len = data.len(),
                                peer_addr = %peer_addr,
                                self_addr = %self_addr,
                                "Received some data from the agent",
                            );

                            stream.write_all(&data).await?;
                        }
                    },
                },

                _ = time::sleep(Self::MIRROR_MODE_LINGER_TIMEOUT), if is_lingering => {
                    tracing::trace!(
                        peer_addr = %peer_addr,
                        self_addr = %self_addr,
                        "Message bus is closed and the connection is silent, exiting",
                    );

                    break Ok(());
                }
            }
        }
    }
}
