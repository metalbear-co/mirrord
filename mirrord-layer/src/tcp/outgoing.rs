use std::{collections::HashMap, net::SocketAddr};

use futures::SinkExt;
use mirrord_protocol::{
    tcp::LayerTcp, ClientCodec, ClientMessage, ConnectRequest, ConnectResponse,
    OutgoingTrafficRequest, OutgoingTrafficResponse,
};
use tokio::net::{TcpListener, TcpStream};
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
pub(crate) enum OutgoingTraffic {
    Connect(Connect),
}

#[derive(Debug)]
pub(crate) struct OutgoingTrafficHandler {
    mirror_list: HashMap<i32, TcpStream>,
    connect_queue: ResponseDeque<ConnectResponse>,
}

impl Default for OutgoingTrafficHandler {
    fn default() -> Self {
        Self {
            mirror_list: HashMap::with_capacity(4),
            connect_queue: ResponseDeque::with_capacity(4),
        }
    }
}

impl OutgoingTrafficHandler {
    pub(crate) async fn handle_hook_message(
        &mut self,
        message: OutgoingTraffic,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<(), LayerError> {
        match message {
            OutgoingTraffic::Connect(connect) => {
                let Connect {
                    remote_address,
                    mirror_listener,
                    channel_tx,
                    user_fd,
                } = connect;

                let (mirror_stream, _) = mirror_listener.accept().await?;
                self.mirror_list.insert(user_fd, mirror_stream);

                self.connect_queue.push_back(channel_tx);

                Ok(codec
                    .send(ClientMessage::OutgoingTraffic(
                        OutgoingTrafficRequest::Connect(ConnectRequest { remote_address }),
                    ))
                    .await?)
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
        }
    }
}
