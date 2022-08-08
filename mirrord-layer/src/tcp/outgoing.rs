use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::prelude::AsRawFd,
    sync::atomic::Ordering,
};

use futures::SinkExt;
use mirrord_protocol::{
    ClientCodec, ClientMessage, ConnectRequest, ConnectResponse, OutgoingTrafficRequest,
    OutgoingTrafficResponse, ReadResponse, WriteRequest,
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
    common::{ResponseChannel, ResponseDeque},
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

#[derive(Debug)]
pub(crate) struct CreateMirrorStream {
    pub(crate) user_fd: i32,
    pub(crate) mirror_listener: std::net::TcpListener,
}

#[derive(Debug)]
pub(crate) enum OutgoingTraffic {
    Connect(Connect),
}

#[derive(Debug)]
pub(crate) struct OutgoingTrafficHandler {
    // task: task::JoinHandle<Result<(), LayerError>>,
    read_buffer: Vec<u8>,
    mirrors: HashMap<i32, ConnectionMirror>,
    mirror_streams: HashMap<i32, TcpStream>,
    connect_queue: ResponseDeque<MirrorConnect>,
}

#[derive(Debug)]
pub(crate) struct ConnectionMirror {
    sender: tokio::sync::mpsc::Sender<Vec<u8>>,
}

impl Default for OutgoingTrafficHandler {
    fn default() -> Self {
        Self {
            read_buffer: Vec::with_capacity(1500),
            mirrors: HashMap::with_capacity(4),
            mirror_streams: HashMap::with_capacity(4),
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
impl OutgoingTrafficHandler {
    async fn run(mut mirror_stream: TcpStream, remote_stream: Receiver<Vec<u8>>) {
        // TODO(alex) [high] 2022-07-20:
        // 1. Take ownership of streams;
        // 2. Loop forever;
        // 3. Handle hook messages;
        // 4. Call `recv` on `mirror_streams`;
        // 5. Send data received as `ClientMessage::Write`;
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
                            warn!("tcp_tunnel -> exiting due to local stream closed!");
                            break;
                        },
                        Ok(_) => {
                            // TODO(alex) [high] 2022-08-08: Send the data we received here to
                            // `agent`.
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
        message: OutgoingTraffic,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<(), LayerError> {
        trace!(
            "OutgoingTrafficHandler::handle_hook_message -> message {:#?}",
            message
        );

        for (id, mirror) in self.mirror_streams.iter_mut() {
            let read_amount = mirror.read(&mut self.read_buffer).await?;

            codec
                .send(ClientMessage::OutgoingTraffic(
                    OutgoingTrafficRequest::Write(WriteRequest {
                        id: *id,
                        bytes: self.read_buffer[..read_amount].to_vec(),
                    }),
                ))
                .await?;
        }

        match message {
            OutgoingTraffic::Connect(Connect {
                remote_address,
                channel_tx,
                // TODO(alex) [mid] 2022-07-20: Has to be socket address, rather than fd?
                user_fd,
            }) => {
                trace!(
                    "OutgoingTraffic::Connect -> remote_address {:#?}",
                    remote_address,
                );

                self.connect_queue.push_back(channel_tx);

                Ok(codec
                    .send(ClientMessage::OutgoingTraffic(
                        OutgoingTrafficRequest::Connect(ConnectRequest {
                            user_fd,
                            remote_address,
                        }),
                    ))
                    .await?)
            }
        }
    }

    pub(crate) async fn handle_daemon_message(
        &mut self,
        response: OutgoingTrafficResponse,
    ) -> Result<(), LayerError> {
        trace!(
            "OutgoingTraffic::handle_daemon_message -> message {:?}",
            response
        );

        match response {
            OutgoingTrafficResponse::Connect(connect) => {
                let ConnectResponse { user_fd } = connect?;

                debug!(
                    "OutgoingTraffic::handle_daemon_message -> usef_fd {:#?}",
                    user_fd
                );

                IS_INTERNAL_CALL.store(true, Ordering::Release);

                debug!("OutgoingTraffic::handle_daemon_message -> before binding");
                // TODO(alex) [mid] 2022-08-08: This must match the `family` of the original
                // request, meaning that, if the user tried to connect with Ipv4, this should be
                // an Ipv4 (same for Ipv6).
                let mirror_listener =
                    TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?;
                debug!(
                    "OutgoingTraffic::handle_daemon_message -> mirror_listener {:#?}",
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
                    "OutgoingTraffic::handle_daemon_message -> mirror_stream {:#?}",
                    mirror_stream
                );

                IS_INTERNAL_CALL.store(false, Ordering::Release);

                let (sender, receiver) = channel::<Vec<u8>>(1000);

                // TODO(alex) [high] 2022-08-08: Should be very similar to `handle_new_connection`.
                self.mirrors.insert(user_fd, ConnectionMirror { sender });

                task::spawn(OutgoingTrafficHandler::run(mirror_stream, receiver));

                Ok(())
            }
            OutgoingTrafficResponse::Read(read) => {
                // This means that `agent` read something from remote, so we write it to the `user`.
                let ReadResponse { id, bytes } = read?;

                let mirror_stream = self
                    .mirror_streams
                    .get_mut(&id)
                    .ok_or(LayerError::LocalFDNotFound(id))?;

                mirror_stream.write(&bytes).await?;

                Ok(())
            }
            OutgoingTrafficResponse::Write(_) => {
                // TODO(alex) [high] 2022-07-20: Receive message from agent.
                todo!();
            }
        }
    }
}
