use core::fmt;
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::atomic::Ordering,
};

use futures::SinkExt;
use mirrord_protocol::{
    tcp::outgoing::{
        ConnectRequest, ConnectResponse, ReadResponse, TcpOutgoingRequest, TcpOutgoingResponse,
        WriteRequest, WriteResponse,
    },
    ClientCodec, ClientMessage,
};
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
    error::LayerError,
    socket::ops::IS_INTERNAL_CALL,
};

#[derive(Debug)]
pub(crate) struct MirrorConnect {
    pub(crate) mirror_address: SocketAddr,
}

#[derive(Debug)]
pub(crate) struct Connect {
    pub(crate) remote_address: SocketAddr,
    pub(crate) channel_tx: ResponseChannel<MirrorConnect>,
    pub(crate) user_fd: i32,
}

pub(crate) struct Write {
    pub(crate) id: i32,
    pub(crate) bytes: Vec<u8>,
}

impl fmt::Debug for Write {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Write")
            .field("id", &self.id)
            .field("bytes (length)", &self.bytes.len())
            .finish()
    }
}

#[derive(Debug)]
pub(crate) enum TcpOutgoing {
    Connect(Connect),
    Write(Write),
}

#[derive(Debug)]
pub(crate) struct TcpOutgoingHandler {
    mirrors: HashMap<i32, ConnectionMirror>,
    connect_queue: ResponseDeque<MirrorConnect>,
}

#[derive(Debug)]
pub(crate) struct ConnectionMirror {
    sender: tokio::sync::mpsc::Sender<Vec<u8>>,
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
        id: i32,
        mut mirror_stream: TcpStream,
        remote_stream: Receiver<Vec<u8>>,
    ) {
        let mut remote_stream = ReceiverStream::new(remote_stream);
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
                            // TODO(alex) [mid] 2022-08-09: Use hook channel or create some other
                            // new channel just to handle these types of messages?

                            // Sends the message that the user wrote to our interceptor socket to
                            // be handled on the `agent`, where it'll be forwarded to the remote.
                            let write = Write { id, bytes: buffer[..read_amount].to_vec() };
                            let outgoing_data = TcpOutgoing::Write(write);

                            // TODO(alex) [mid] 2022-08-09: Must handle a response from `agent`
                            // mostly for the case where `send` failed sending data to remote.
                            // Need a `written_amount` or error type of response to mimick how
                            // proper `io` works.
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
                            warn!("tcp_tunnel -> exiting due to remote stream closed!");
                            break;
                        }
                    }
                },
            }
        }
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
                // TODO(alex) [mid] 2022-07-20: Has to be socket address, rather than fd?
                user_fd,
            }) => {
                trace!("Connect -> remote_address {:#?}", remote_address,);

                self.connect_queue.push_back(channel_tx);

                Ok(codec
                    .send(ClientMessage::TcpOutgoing(TcpOutgoingRequest::Connect(
                        ConnectRequest {
                            user_fd,
                            remote_address,
                        },
                    )))
                    .await?)
            }
            TcpOutgoing::Write(Write { id, bytes }) => {
                trace!("Write -> id {:#?} | bytes (len) {:#?}", id, bytes.len(),);

                Ok(codec
                    .send(ClientMessage::TcpOutgoing(TcpOutgoingRequest::Write(
                        WriteRequest { id, bytes },
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
                    user_fd,
                    remote_address,
                } = connect?;

                debug!("handle_daemon_message -> usef_fd {:#?}", user_fd);

                IS_INTERNAL_CALL.store(true, Ordering::Release);

                debug!("handle_daemon_message -> before binding");
                // TODO(alex) [mid] 2022-08-08: This must match the `family` of the original
                // request, meaning that, if the user tried to connect with Ipv4, this should be
                // an Ipv4 (same for Ipv6).
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

                debug!(
                    "handle_daemon_message -> mirror_listener {:#?}",
                    mirror_listener
                );

                {
                    let mirror_address = mirror_listener.local_addr()?;
                    let mirror_connect = MirrorConnect { mirror_address };

                    let _ = self
                        .connect_queue
                        .pop_front()
                        .ok_or(LayerError::SendErrorTcpResponse)?
                        .send(Ok(mirror_connect))
                        .map_err(|_| LayerError::SendErrorTcpResponse)?;
                }

                let (mirror_stream, user_address) = mirror_listener.accept().await?;
                debug!(
                    "handle_daemon_message -> mirror_stream {:#?}",
                    mirror_stream
                );

                IS_INTERNAL_CALL.store(false, Ordering::Release);

                let (sender, receiver) = channel::<Vec<u8>>(1000);

                // TODO(alex) [high] 2022-08-08: Should be very similar to `handle_new_connection`.
                self.mirrors.insert(user_fd, ConnectionMirror { sender });

                task::spawn(TcpOutgoingHandler::interceptor_task(
                    user_fd,
                    mirror_stream,
                    receiver,
                ));

                Ok(())
            }
            TcpOutgoingResponse::Read(read) => {
                trace!("Read -> read {:?}", read);
                // `agent` read something from remote, so we write it to the `user`.
                let ReadResponse { id, bytes } = read?;

                let sender = self
                    .mirrors
                    .get_mut(&id)
                    .ok_or(LayerError::LocalFDNotFound(id))
                    .map(|mirror| &mut mirror.sender)?;

                Ok(sender.send(bytes).await?)
            }
            TcpOutgoingResponse::Write(write) => {
                trace!("Write -> write {:?}", write);

                let WriteResponse { id } = write?;
                // TODO(alex) [mid] 2022-07-20: Receive message from agent.
                // ADD(alex) [mid] 2022-08-09: Should be very similar to `Read`, but `sender` works
                // with `Vec<u8>`, so maybe wrapping it in a `Result<Vec<u8>, Error>` would make
                // more sense, and enable this idea.

                Ok(())
            }
        }
    }
}
