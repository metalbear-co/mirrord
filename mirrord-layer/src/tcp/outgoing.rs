use core::fmt;
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::prelude::AsRawFd,
};

use futures::SinkExt;
use mirrord_protocol::{tcp::outgoing::*, ClientCodec, ClientMessage, ConnectionId};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{channel, Receiver},
    task,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::{debug, error, trace, warn};

use crate::{
    common::{send_hook_message, HookMessage, ResponseChannel, ResponseDeque},
    detour::DetourGuard,
    error::LayerError,
    socket::OUTGOING_SOCKETS,
};

#[derive(Debug)]
pub(crate) struct MirrorConnect {
    pub(crate) mirror_address: SocketAddr,
}

#[derive(Debug)]
pub(crate) struct Connect {
    pub(crate) remote_address: SocketAddr,
    pub(crate) channel_tx: ResponseChannel<MirrorConnect>,
}

pub(crate) struct Write {
    pub(crate) connection_id: ConnectionId,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct Close {
    pub(crate) connection_id: ConnectionId,
}

impl fmt::Debug for Write {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Write")
            .field("id", &self.connection_id)
            .field("bytes (length)", &self.bytes.len())
            .finish()
    }
}

#[derive(Debug)]
pub(crate) enum TcpOutgoing {
    Connect(Connect),
    Write(Write),
    Close(Close),
}

#[derive(Debug)]
pub(crate) struct TcpOutgoingHandler {
    mirrors: HashMap<ConnectionId, ConnectionMirror>,
    connect_queue: ResponseDeque<MirrorConnect>,
}

#[derive(Debug)]
pub(crate) struct ConnectionMirror {
    remote_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
}

impl Default for TcpOutgoingHandler {
    fn default() -> Self {
        Self {
            mirrors: HashMap::with_capacity(4),
            connect_queue: ResponseDeque::with_capacity(4),
        }
    }
}

// TODO(alex) [high] 2022-08-08: Need something very similar to `TcpMirrorHandler`, where we
// separate a task that keeps reading from the user stream, reading from the remote stream, and
// sends what has been (local) read to agent as a message.
// Something like:
//
// - (local) write [user] -> (mirror) read -> client message -> daemon response -> (mirror) write ->
//   (local) read [user]
//
// - (remote) write [out] -> daemon response -> (mirror) read -> (mirror) write ->
// (local) read [user]
impl TcpOutgoingHandler {
    /// TODO(alex) [low] 2022-08-09: Document this function.
    async fn interceptor_task(
        connection_id: ConnectionId,
        mut mirror_stream: TcpStream,
        remote_rx: Receiver<Vec<u8>>,
    ) {
        let mut remote_stream = ReceiverStream::new(remote_rx);
        let mut buffer = vec![0; 1024];

        loop {
            select! {
                biased; // To allow local socket to be read before being closed

                // Reads data that the user is sending from their socket to mirrord's interceptor
                // socket.
                read = mirror_stream.read(&mut buffer) => {
                    match read {
                        Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => {
                            continue;
                        },
                        Err(fail) => {
                            error!("Failed reading mirror_stream with {:#?}", fail);
                            break;
                        }
                        Ok(read_amount) if read_amount == 0 => {
                            warn!("interceptor_task -> exiting due to local stream closed!");
                            break;
                        },
                        Ok(read_amount) => {
                            // Sends the message that the user wrote to our interceptor socket to
                            // be handled on the `agent`, where it'll be forwarded to the remote.
                            let write = Write { connection_id, bytes: buffer[..read_amount].to_vec() };
                            let outgoing_data = TcpOutgoing::Write(write);

                            if let Err(fail) = send_hook_message(HookMessage::TcpOutgoing(outgoing_data)).await {
                                error!("Failed sending write message with {:#?}!", fail);
                                break;
                            }
                        }
                    }
                },
                bytes = remote_stream.next() => {
                    match bytes {
                        Some(bytes) => {
                            // Writes the data sent by `agent` (that came from the actual remote
                            // stream) to our interceptor socket. When the user tries to read the
                            // remote data, this'll be what they receive.
                            if let Err(fail) = mirror_stream.write_all(&bytes).await {
                                error!("Failed writing to mirror_stream with {:#?}!", fail);
                                break;
                            }
                        },
                        None => {
                            warn!("interceptor_task -> exiting due to remote stream closed!");
                            break;
                        }
                    }
                },
            }
        }

        trace!(
            "interceptor_task done -> connection_id {:#?}",
            connection_id
        );
    }

    pub(crate) async fn handle_hook_message(
        &mut self,
        message: TcpOutgoing,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<(), LayerError> {
        trace!("handle_hook_message -> message {:?}", message);

        match message {
            TcpOutgoing::Connect(Connect {
                remote_address,
                channel_tx,
            }) => {
                trace!("Connect -> remote_address {:#?}", remote_address,);

                self.connect_queue.push_back(channel_tx);

                Ok(codec
                    .send(ClientMessage::TcpOutgoing(TcpOutgoingRequest::Connect(
                        ConnectRequest { remote_address },
                    )))
                    .await?)
            }
            TcpOutgoing::Write(Write {
                connection_id,
                bytes,
            }) => {
                trace!(
                    "Write -> connection_id {:#?} | bytes (len) {:#?}",
                    connection_id,
                    bytes.len(),
                );

                Ok(codec
                    .send(ClientMessage::TcpOutgoing(TcpOutgoingRequest::Write(
                        WriteRequest {
                            connection_id,
                            bytes,
                        },
                    )))
                    .await?)
            }
            TcpOutgoing::Close(Close { connection_id }) => {
                trace!("Close -> connection_id {:#?}", connection_id);

                Ok(codec
                    .send(ClientMessage::TcpOutgoing(TcpOutgoingRequest::Close(
                        CloseRequest { connection_id },
                    )))
                    .await?)
            }
        }
    }

    pub(crate) async fn handle_daemon_message(
        &mut self,
        response: TcpOutgoingResponse,
    ) -> Result<(), LayerError> {
        trace!("handle_daemon_message -> message {:?}", response);

        match response {
            TcpOutgoingResponse::Connect(connect) => {
                trace!("Connect -> connect {:#?}", connect);

                let ConnectResponse {
                    connection_id,
                    remote_address,
                } = connect?;

                debug!(
                    "handle_daemon_message -> connection_id {:#?}",
                    connection_id
                );

                let mirror_stream = {
                    let _ = DetourGuard::new();

                    let mirror_listener = match remote_address {
                        SocketAddr::V4(_) => {
                            TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
                                .await?
                        }
                        SocketAddr::V6(_) => {
                            TcpListener::bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0))
                                .await?
                        }
                    };

                    let mirror_address = mirror_listener.local_addr()?;
                    let mirror_connect = MirrorConnect { mirror_address };

                    let _ = self
                        .connect_queue
                        .pop_front()
                        .ok_or(LayerError::SendErrorTcpResponse)?
                        .send(Ok(mirror_connect))
                        .map_err(|_| LayerError::SendErrorTcpResponse)?;

                    let (mirror_stream, _) = mirror_listener.accept().await?;
                    mirror_stream
                };

                let (remote_tx, remote_rx) = channel::<Vec<u8>>(1000);

                self.mirrors
                    .insert(connection_id, ConnectionMirror { remote_tx });

                OUTGOING_SOCKETS
                    .lock()?
                    .insert(mirror_stream.as_raw_fd(), connection_id);

                task::spawn(TcpOutgoingHandler::interceptor_task(
                    connection_id,
                    mirror_stream,
                    remote_rx,
                ));

                Ok(())
            }
            TcpOutgoingResponse::Read(read) => {
                trace!("Read -> read {:?}", read);
                // `agent` read something from remote, so we write it to the `user`.
                let ReadResponse {
                    connection_id,
                    bytes,
                } = read?;

                let sender = self
                    .mirrors
                    .get_mut(&connection_id)
                    .ok_or(LayerError::NoConnectionId(connection_id))
                    .map(|mirror| &mut mirror.remote_tx)?;

                Ok(sender.send(bytes).await?)
            }
            TcpOutgoingResponse::Write(write) => {
                trace!("Write -> write {:?}", write);

                let WriteResponse { .. } = write?;

                Ok(())
            }
        }
    }
}
