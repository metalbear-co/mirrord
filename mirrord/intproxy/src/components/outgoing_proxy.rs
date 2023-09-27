// use std::marker::PhantomData;

// use mirrord_protocol::{
//     outgoing::{tcp::LayerTcpOutgoing, udp::LayerUdpOutgoing, LayerConnect},
//     ClientMessage,
// };

// use crate::{
//     agent_conn::AgentSender,
//     error::Result,
//     layer_conn::LayerSender,
//     protocol::{ConnectOutgoing, MessageId, NetProtocol, Tcp, Udp},
//     request_queue::RequestQueue,
// };

// pub struct OutgoingProxy<P> {
//     layer_sender: LayerSender,
//     agent_sender: AgentSender,
//     connect_queue: RequestQueue,
//     _phantom: PhantomData<fn() -> P>,
// }

// impl<P: NetProtocolExt> OutgoingProxy<P> {
//     pub fn new(layer_sender: LayerSender, agent_sender: AgentSender) -> Self {
//         Self {
//             layer_sender,
//             agent_sender,
//             connect_queue: Default::default(),
//             _phantom: Default::default(),
//         }
//     }

//     pub async fn handle_request(
//         &mut self,
//         connect: ConnectOutgoing<P>,
//         message_id: MessageId,
//     ) -> Result<()> { self.connect_queue.save_request_id(message_id);

//         let message = P::wrap_layer_connect(LayerConnect {
//             remote_address: connect.remote_address,
//         });
//         self.agent_sender.send(message).await.map_err(Into::into)
//     }
// }

// pub trait NetProtocolExt: NetProtocol {
//     fn wrap_layer_connect(message: LayerConnect) -> ClientMessage;
// }

// impl NetProtocolExt for Udp {
//     fn wrap_layer_connect(message: LayerConnect) -> ClientMessage {
//         ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(message))
//     }
// }

// impl NetProtocolExt for Tcp {
//     fn wrap_layer_connect(message: LayerConnect) -> ClientMessage {
//         ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(message))
//     }
// }

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use mirrord_protocol::{
    outgoing::{
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
        DaemonConnect, DaemonRead, LayerConnect, SocketAddress, LayerClose, LayerWrite,
    },
    ClientMessage, ConnectionId, RemoteResult,
    ResponseError::NotImplemented,
};
use tokio::{
    net::UdpSocket,
    sync::mpsc::{channel, Receiver, Sender},
    task,
};
use crate::{
    agent_conn::AgentSender,
    error::{IntProxyError, Result},
    layer_conn::LayerSender,
    protocol::{
        ConnectOutgoing, LocalMessage, MessageId, OutgoingConnectResponse, ProxyToLayerMessage, Udp,
    },
    request_queue::RequestQueue,
};

#[derive(Clone)]
struct InterceptorSocketSender(Sender<Vec<u8>>);

impl InterceptorSocketSender {
    async fn send(&self, data: Vec<u8>) -> Result<()> {
        self.0.send(data).await.map_err(|_| todo!())
    }
}

/// Responsible for handling hook and daemon messages for the outgoing traffic feature.
pub struct UdpOutgoingHandler {
    agent_sender: AgentSender,
    layer_sender: LayerSender,

    mirrors: HashMap<ConnectionId, InterceptorSocketSender>,

    connect_queue: RequestQueue,
    // /// Channel used to pass messages (currently only `Write`) from an intercepted socket to the
    // /// main `layer` loop.
    // ///
    // /// This is sent from `interceptor_task`.
    // layer_tx: Sender<LayerUdpOutgoing>,
    // layer_rx: Receiver<LayerUdpOutgoing>,
}

impl UdpOutgoingHandler {
    async fn interceptor_task(
        agent_sender: AgentSender,
        connection_id: ConnectionId,
        mirror_socket: UdpSocket,
        mut remote_rx: Receiver<Vec<u8>>,
    ) {
        let close_remote_stream = |agent_sender: AgentSender| async move {
            let close = LayerClose { connection_id };
            let outgoing_close = LayerUdpOutgoing::Close(close);

            if let Err(e) = agent_sender.send(ClientMessage::UdpOutgoing(outgoing_close)).await {
                tracing::error!("Failed sending close message with {:#?}!", e);
            }
        };

        let mut recv_from_buffer = vec![0; 1500];
        let mut user_address: Option<SocketAddr> = None;

        loop {
            tokio::select! {
                biased; // To allow local socket to be read before being closed

                read = mirror_socket.recv_from(&mut recv_from_buffer) => {
                    match read {
                        Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => {
                            continue;
                        },
                        Err(fail) => {
                            tracing::info!("failed reading mirror_stream with {:#?}", fail);
                            close_remote_stream(agent_sender).await;
                            break;
                        }
                        Ok((0, _)) => {
                            tracing::trace!("interceptor_task -> Stream {:#?} has no more data, closing!", connection_id);
                            close_remote_stream(agent_sender).await;
                            break;
                        },
                        Ok((read_amount, from)) => {
                            tracing::trace!("interceptor_task -> Received data from user socket {:#?}", from);
                            user_address = Some(from);
                            let write = LayerWrite {
                                connection_id,
                                bytes: recv_from_buffer
                                    .get(..read_amount)
                                    .expect("recv_from returned more bytes than the buffer can hold")
                                    .to_vec(),
                            };
                            let outgoing_write = LayerUdpOutgoing::Write(write);

                            if let Err(err) = agent_sender.send(ClientMessage::UdpOutgoing(outgoing_write)).await {
                                tracing::error!("Failed sending write message with {err:#?}!");
                                break;
                            }
                        }
                    }
                },
                bytes = remote_rx.recv() => {
                    match bytes {
                        Some(bytes) => {
                            tracing::trace!("interceptor_task -> Received data from remote socket");
                            // Writes the data sent by `agent` (that came from the actual remote
                            // stream) to our interceptor socket. When the user tries to read the
                            // remote data, this'll be what they receive.
                            if let Err(fail) = mirror_socket
                                .send_to(
                                    &bytes,
                                    user_address.expect("User socket should be set by now!"),
                                )
                                .await
                            {
                                tracing::trace!("Failed writing to mirror_stream with {:#?}!", fail);
                                break;
                            }
                        },
                        None => {
                            tracing::warn!("interceptor_task -> exiting due to remote stream closed!");
                            break;
                        }
                    }
                },
            }
        }

        tracing::trace!(
            "interceptor_task done -> connection_id {:#?}",
            connection_id
        );
    }

    pub async fn handle_layer_request(
        &mut self,
        message: ConnectOutgoing<Udp>,
        message_id: MessageId,
    ) -> Result<()> {
        self.connect_queue.save_request_id(message_id);
        self.agent_sender
            .send(ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(
                LayerConnect {
                    remote_address: message.remote_address,
                },
            )))
            .await?;

        Ok(())
    }

    pub async fn handle_agent_message(&mut self, message: DaemonUdpOutgoing) -> Result<()> {
        match message {
            DaemonUdpOutgoing::Connect(connect) => self.handle_agent_connect(connect).await,
            DaemonUdpOutgoing::Read(read) => self.handle_agent_read(read).await,
            DaemonUdpOutgoing::Close(connection_id) => {
                self.handle_agent_close(connection_id);
                Ok(())
            }
        }
    }

    async fn handle_agent_connect(&mut self, connect: RemoteResult<DaemonConnect>) -> Result<()> {
        let message_id = self
            .connect_queue
            .get_request_id()
            .ok_or(IntProxyError::RequestQueueEmpty)?;

        let connect = match connect {
            Ok(connect) => connect,
            Err(e) => {
                return self
                    .layer_sender
                    .send(LocalMessage {
                        message_id,
                        inner: ProxyToLayerMessage::ConnectUdpOutgoing(Err(e)),
                    })
                    .await
                    .map_err(Into::into)
            }
        };

        let DaemonConnect {
            connection_id,
            remote_address,
            local_address,
        } = connect;

        let mirror_socket = match remote_address {
            SocketAddress::Ip(SocketAddr::V4(_)) => {
                UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
            }
            SocketAddress::Ip(SocketAddr::V6(_)) => {
                UdpSocket::bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0))
            }
            SocketAddress::Unix(_) => {
                // This should never happen. If we're here - the agent reported
                // a UDP connection to a remote unix socket. The layer does not
                // request such a thing, so either there was a bug earlier in
                // the layer, the agent is rogue, or this code is outdated.
                tracing::error!("Datagrams over unix sockets are not supported.");
                Err(NotImplemented)?
            }
        }
        .await?;

        let (remote_tx, remote_rx) = channel::<Vec<u8>>(1000);

        let layer_address = mirror_socket.local_addr()?;

        // user and interceptor sockets are connected to each other, so now we spawn
        // a new task to pair their reads/writes.
        task::spawn(UdpOutgoingHandler::interceptor_task(
            self.agent_sender.clone(),
            connection_id,
            mirror_socket,
            remote_rx,
        ));

        self.mirrors.insert(connection_id, InterceptorSocketSender(remote_tx));

        let response = Ok(OutgoingConnectResponse {
            user_app_address: local_address,
            layer_address: layer_address.into(),
        });

        self.layer_sender
            .send(LocalMessage {
                message_id,
                inner: ProxyToLayerMessage::ConnectUdpOutgoing(response),
            })
            .await
            .map_err(Into::into)
    }

    async fn handle_agent_read(&mut self, read: RemoteResult<DaemonRead>) -> Result<()> {
        let DaemonRead {
            connection_id,
            bytes,
        } = read?;

        let sender = self
            .mirrors
            .get_mut(&connection_id)
            .ok_or(IntProxyError::NoConnectionId(connection_id))?;

        Ok(sender.send(bytes).await?)
    }

    fn handle_agent_close(&mut self, connection_id: u64) {
        self.mirrors.remove(&connection_id);
    }
}
