use std::collections::HashMap;

use mirrord_protocol::{
    outgoing::{tcp::DaemonTcpOutgoing, udp::DaemonUdpOutgoing, DaemonConnect, DaemonRead},
    ConnectionId, RemoteResult,
};

use crate::{
    agent_conn::AgentSender,
    error::{IntProxyError, Result},
    layer_conn::LayerSender,
    protocol::{
        LocalMessage, MessageId, NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse,
        ProxyToLayerMessage,
    },
    proxies::outgoing::interceptor::{InterceptorTask, InterceptorTaskHandle},
    request_queue::RequestQueue,
};

pub struct OutgoingProxy {
    agent_sender: AgentSender,
    layer_sender: LayerSender,
    interceptors: HashMap<(ConnectionId, NetProtocol), InterceptorTaskHandle>,
    connect_queues: HashMap<NetProtocol, RequestQueue>,
}

impl OutgoingProxy {
    pub fn new(agent_sender: AgentSender, layer_sender: LayerSender) -> Self {
        Self {
            agent_sender,
            layer_sender,
            interceptors: Default::default(),
            connect_queues: Default::default(),
        }
    }

    pub async fn handle_layer_connect_request(
        &mut self,
        message: OutgoingConnectRequest,
        message_id: MessageId,
    ) -> Result<()> {
        let OutgoingConnectRequest {
            remote_address,
            protocol,
        } = message;

        self.connect_queues
            .entry(protocol)
            .or_default()
            .save_request_id(message_id);

        self.agent_sender
            .send(protocol.wrap_agent_connect(remote_address))
            .await
            .map_err(Into::into)
    }

    pub async fn handle_agent_tcp_message(&mut self, message: DaemonTcpOutgoing) -> Result<()> {
        match message {
            DaemonTcpOutgoing::Close(connection_id) => {
                self.handle_agent_close(connection_id, NetProtocol::Stream);
                Ok(())
            }
            DaemonTcpOutgoing::Connect(connect) => {
                self.handle_agent_connect(connect, NetProtocol::Stream)
                    .await
            }
            DaemonTcpOutgoing::Read(read) => {
                self.handle_agent_read(read, NetProtocol::Stream).await
            }
        }
    }

    pub async fn handle_agent_udp_message(&mut self, message: DaemonUdpOutgoing) -> Result<()> {
        match message {
            DaemonUdpOutgoing::Close(connection_id) => {
                self.handle_agent_close(connection_id, NetProtocol::Datagrams);
                Ok(())
            }
            DaemonUdpOutgoing::Connect(connect) => {
                self.handle_agent_connect(connect, NetProtocol::Datagrams)
                    .await
            }
            DaemonUdpOutgoing::Read(read) => {
                self.handle_agent_read(read, NetProtocol::Datagrams).await
            }
        }
    }

    fn handle_agent_close(&mut self, connection_id: ConnectionId, protocol: NetProtocol) {
        self.interceptors.remove(&(connection_id, protocol));
    }

    async fn handle_agent_read(
        &mut self,
        read: RemoteResult<DaemonRead>,
        protocol: NetProtocol,
    ) -> Result<()> {
        let DaemonRead {
            connection_id,
            bytes,
        } = read?;

        let sender = self
            .interceptors
            .get_mut(&(connection_id, protocol))
            .ok_or(IntProxyError::NoConnectionId(connection_id))?;

        let _ = sender.send(bytes).await;

        Ok(())
    }

    async fn handle_agent_connect(
        &mut self,
        connect: RemoteResult<DaemonConnect>,
        protocol: NetProtocol,
    ) -> Result<()> {
        let message_id = self
            .connect_queues
            .get_mut(&protocol)
            .and_then(RequestQueue::get_request_id)
            .ok_or(IntProxyError::RequestQueueEmpty)?;

        let connect = match connect {
            Ok(connect) => connect,
            Err(e) => {
                return self
                    .layer_sender
                    .send(LocalMessage {
                        message_id,
                        inner: ProxyToLayerMessage::OutgoingConnect(Err(e)),
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

        let prepared_socket = protocol.prepare_socket(remote_address).await?;
        let layer_address = prepared_socket.local_address()?;

        let task = InterceptorTask::new(self.agent_sender.clone(), connection_id, protocol, 512);
        let handle = task.handle();
        self.interceptors.insert((connection_id, protocol), handle);

        tokio::spawn(task.run(prepared_socket));

        self.layer_sender
            .send(LocalMessage {
                message_id,
                inner: ProxyToLayerMessage::OutgoingConnect(Ok(OutgoingConnectResponse {
                    layer_address,
                    in_cluster_address: local_address,
                })),
            })
            .await
            .map_err(Into::into)
    }
}
