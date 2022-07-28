use std::{
    collections::HashMap, net::SocketAddr, os::unix::prelude::AsRawFd, sync::atomic::Ordering,
};

use futures::SinkExt;
use mirrord_protocol::{
    ClientCodec, ClientMessage, ConnectRequest, ConnectResponse, OutgoingTrafficRequest,
    OutgoingTrafficResponse, ReadResponse, WriteRequest,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tracing::trace;

use crate::{
    common::{ResponseChannel, ResponseDeque},
    error::LayerError,
    socket::ops::IS_INTERNAL_CALL,
};

#[derive(Debug)]
pub(crate) struct Connect {
    pub(crate) remote_address: SocketAddr,
    pub(crate) channel_tx: ResponseChannel<ConnectResponse>,
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
    CreateMirrorStream(CreateMirrorStream),
}

#[derive(Debug)]
pub(crate) struct OutgoingTrafficHandler {
    // task: task::JoinHandle<Result<(), LayerError>>,
    read_buffer: Vec<u8>,
    mirrors: HashMap<i32, MirrorStream>,
    mirror_streams: HashMap<i32, TcpStream>,
    connect_queue: ResponseDeque<ConnectResponse>,
}

#[derive(Debug)]
pub(crate) struct MirrorStream {
    mirror: TcpStream,
}

impl Default for OutgoingTrafficHandler {
    fn default() -> Self {
        // let task = task::spawn(Self::run());

        Self {
            // task,
            read_buffer: Vec::with_capacity(1500),
            mirrors: HashMap::with_capacity(4),
            mirror_streams: HashMap::with_capacity(4),
            connect_queue: ResponseDeque::with_capacity(4),
        }
    }
}

impl OutgoingTrafficHandler {
    async fn run() -> Result<(), LayerError> {
        // TODO(alex) [high] 2022-07-20:
        // 1. Take ownership of streams;
        // 2. Loop forever;
        // 3. Handle hook messages;
        // 4. Call `recv` on `mirror_streams`;
        // 5. Send data received as `ClientMessage::Write`;
        todo!()
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

                // TODO(alex) [high] 2022-07-28: Move this handling to the response part, there we
                // should call a bypass version of `connect` on the user socket, and then call
                // the bypass version of `accept` on our middle socket.
                //
                // Some of this stuff is being done in `ops::connect`, so probably requires moving
                // it around.

                self.connect_queue.push_back(channel_tx);

                Ok(codec
                    .send(ClientMessage::OutgoingTraffic(
                        OutgoingTrafficRequest::Connect(ConnectRequest { remote_address }),
                    ))
                    .await?)
            }
            OutgoingTraffic::CreateMirrorStream(CreateMirrorStream {
                user_fd,
                mirror_listener,
            }) => {
                IS_INTERNAL_CALL.swap(true, Ordering::Acquire);

                let (mirror_stream, _) = TcpListener::from_std(mirror_listener)?.accept().await?;

                IS_INTERNAL_CALL.swap(false, Ordering::Release);

                self.mirrors.insert(
                    user_fd.as_raw_fd(),
                    MirrorStream {
                        mirror: mirror_stream,
                    },
                );

                Ok(())
            }
        }
    }

    pub(crate) async fn handle_daemon_message(
        &mut self,
        message: OutgoingTrafficResponse,
    ) -> Result<(), LayerError> {
        trace!(
            "OutgoingTraffic::handle_daemon_message -> message {:?}",
            message
        );

        match message {
            OutgoingTrafficResponse::Connect(connect) => self
                .connect_queue
                .pop_front()
                .ok_or(LayerError::SendErrorTcpResponse)?
                .send(connect)
                .map_err(|_| LayerError::SendErrorTcpResponse),
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
