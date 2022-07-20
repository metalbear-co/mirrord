use std::{collections::HashMap, net::SocketAddr, os::unix::prelude::AsRawFd};

use futures::SinkExt;
use mirrord_protocol::{
    tcp::LayerTcp, ClientCodec, ClientMessage, ConnectRequest, ConnectResponse,
    OutgoingTrafficRequest, OutgoingTrafficResponse, ReadResponse,
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    task,
};
use tracing::trace;

use crate::{
    common::{ResponseChannel, ResponseDeque},
    error::LayerError,
};

#[derive(Debug)]
pub(crate) struct Connect {
    pub(crate) remote_address: SocketAddr,
    pub(crate) mirror_listener: TcpListener,
    pub(crate) channel_tx: ResponseChannel<ConnectResponse>,
    pub(crate) user_fd: i32,
}

#[derive(Debug)]
pub(crate) struct UserStream {
    pub(crate) stream: TcpStream,
}

#[derive(Debug)]
pub(crate) enum OutgoingTraffic {
    Connect(Connect),
    UserStream(UserStream),
}

#[derive(Debug)]
pub(crate) struct OutgoingTrafficHandler {
    task: task::JoinHandle<Result<(), LayerError>>,
    user_streams: HashMap<i32, TcpStream>,
    mirror_streams: HashMap<i32, TcpStream>,
    connect_queue: ResponseDeque<ConnectResponse>,
}

impl Default for OutgoingTrafficHandler {
    fn default() -> Self {
        let task = task::spawn(Self::run());

        Self {
            task,
            user_streams: HashMap::with_capacity(4),
            mirror_streams: HashMap::with_capacity(4),
            connect_queue: ResponseDeque::with_capacity(4),
        }
    }
}

impl OutgoingTrafficHandler {
    async fn run() -> Result<(), LayerError> {
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
        match message {
            OutgoingTraffic::Connect(Connect {
                remote_address,
                mirror_listener,
                channel_tx,
                user_fd,
            }) => {
                // TODO(alex) [mid] 2022-07-20: Has to be socket address, rather than fd?

                let (mirror_stream, _) = mirror_listener.accept().await?;
                self.mirror_streams.insert(user_fd, mirror_stream);

                self.connect_queue.push_back(channel_tx);

                Ok(codec
                    .send(ClientMessage::OutgoingTraffic(
                        OutgoingTrafficRequest::Connect(ConnectRequest { remote_address }),
                    ))
                    .await?)
            }
            OutgoingTraffic::UserStream(UserStream { stream }) => {
                self.user_streams.insert(stream.as_raw_fd(), stream);

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
                let ReadResponse { id, bytes } = read?;

                let mirror_stream = self
                    .mirror_streams
                    .get_mut(&id)
                    .ok_or(LayerError::LocalFDNotFound(id))?;

                mirror_stream.write(&bytes).await?;

                // TODO(alex) [high] 2022-07-20: Receive message from agent.
                todo!();
            }
            OutgoingTrafficResponse::Write(_) => {
                // TODO(alex) [high] 2022-07-20: Receive message from agent.
                todo!();
            }
        }
    }
}
