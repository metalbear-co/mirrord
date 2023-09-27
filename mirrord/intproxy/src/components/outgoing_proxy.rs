use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use mirrord_protocol::{
    outgoing::{
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
        DaemonConnect, DaemonRead, LayerConnect, SocketAddress,
    },
    ClientMessage, ConnectionId, RemoteResult,
    ResponseError::NotImplemented,
};
use tokio::{net::UdpSocket, task};

use crate::{
    agent_conn::AgentSender,
    error::{IntProxyError, Result},
    layer_conn::LayerSender,
    protocol::{
        ConnectOutgoing, LocalMessage, MessageId, OutgoingConnectResponse, ProxyToLayerMessage, Udp,
    },
    request_queue::RequestQueue,
};

/// Responsible for handling hook and daemon messages for the outgoing traffic feature.
pub struct UdpOutgoingHandler {
    agent_sender: AgentSender,
    layer_sender: LayerSender,
    mirrors: HashMap<ConnectionId, interceptor::InterceptorTaskHandle>,
    connect_queue: RequestQueue,
}

impl UdpOutgoingHandler {
    pub fn new(layer_sender: LayerSender, agent_sender: AgentSender) -> Self {
        Self {
            agent_sender,
            layer_sender,
            mirrors: Default::default(),
            connect_queue: Default::default(),
        }
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

        let layer_address = mirror_socket.local_addr()?;

        let interceptor_task = interceptor::InterceptorTask::new(
            self.agent_sender.clone(),
            connection_id,
            mirror_socket,
            512,
        );
        let interceptor_handle = interceptor_task.handle();

        // user and interceptor sockets are connected to each other, so now we spawn
        // a new task to pair their reads/writes.
        task::spawn(interceptor_task.run());

        self.mirrors.insert(connection_id, interceptor_handle);

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

mod interceptor {
    use std::net::SocketAddr;

    use mirrord_protocol::{
        outgoing::{udp::LayerUdpOutgoing, LayerClose, LayerWrite},
        ClientMessage, ConnectionId,
    };
    use tokio::{
        net::UdpSocket,
        sync::mpsc::{self, Receiver, Sender},
    };

    use crate::{
        agent_conn::AgentSender,
        error::{IntProxyError, Result},
    };

    pub struct InterceptorTaskHandle(Sender<Vec<u8>>);

    impl InterceptorTaskHandle {
        pub async fn send(&self, data: Vec<u8>) -> Result<()> {
            self.0
                .send(data)
                .await
                .map_err(|_| IntProxyError::OutgoingUdpInterceptorFailed)
        }
    }

    pub struct InterceptorTask {
        agent_sender: AgentSender,
        connection_id: ConnectionId,
        local_socket: UdpSocket,
        task_rx: Receiver<Vec<u8>>,
        task_tx: Option<Sender<Vec<u8>>>,
    }

    impl InterceptorTask {
        pub fn new(
            agent_sender: AgentSender,
            connection_id: ConnectionId,
            local_socket: UdpSocket,
            channel_size: usize,
        ) -> Self {
            let (task_tx, task_rx) = mpsc::channel(channel_size);

            Self {
                agent_sender,
                connection_id,
                local_socket,
                task_rx,
                task_tx: task_tx.into(),
            }
        }

        pub fn handle(&self) -> InterceptorTaskHandle {
            let tx = self
                .task_tx
                .as_ref()
                .expect("interceptor sender should not be dropped before the task is run")
                .clone();
            InterceptorTaskHandle(tx)
        }

        async fn close_remote_stream(&self) {
            let close = LayerClose {
                connection_id: self.connection_id,
            };
            let outgoing_close = LayerUdpOutgoing::Close(close);

            if let Err(e) = self
                .agent_sender
                .send(ClientMessage::UdpOutgoing(outgoing_close))
                .await
            {
                tracing::error!("failed sending close message: {e:?}");
            }
        }

        pub async fn run(mut self) {
            self.task_tx = None;

            let mut recv_from_buffer = vec![0; 1500];
            let mut user_address: Option<SocketAddr> = None;

            loop {
                tokio::select! {
                    biased; // To allow local socket to be read before being closed

                    read = self.local_socket.recv_from(&mut recv_from_buffer) => {
                        match read {
                            Err(fail) if fail.kind() == std::io::ErrorKind::WouldBlock => {
                                continue;
                            },
                            Err(fail) => {
                                tracing::info!("failed reading mirror_stream with {fail:#?}");
                                self.close_remote_stream().await;
                                break;
                            }
                            Ok((0, _)) => {
                                tracing::trace!("interceptor_task -> stream {} has no more data, closing", self.connection_id);
                                self.close_remote_stream().await;
                                break;
                            },
                            Ok((read_amount, from)) => {
                                tracing::trace!("interceptor_task -> received data from user socket {from}");

                                user_address.replace(from);

                                let write = LayerWrite {
                                    connection_id: self.connection_id,
                                    bytes: recv_from_buffer
                                        .get(..read_amount)
                                        .expect("recv_from returned more bytes than the buffer can hold")
                                        .to_vec(),
                                };
                                let outgoing_write = LayerUdpOutgoing::Write(write);

                                if let Err(err) = self.agent_sender.send(ClientMessage::UdpOutgoing(outgoing_write)).await {
                                    tracing::error!("failed sending write message with {err:#?}");
                                    break;
                                }
                            }
                        }
                    },

                    bytes = self.task_rx.recv() => {
                        match bytes {
                            Some(bytes) => {
                                tracing::trace!("interceptor_task -> Received data from remote socket");
                                // Writes the data sent by `agent` (that came from the actual remote
                                // stream) to our interceptor socket. When the user tries to read the
                                // remote data, this'll be what they receive.
                                if let Err(fail) = self
                                    .local_socket
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
                "interceptor_task done -> connection_id {}",
                self.connection_id
            );
        }
    }
}
