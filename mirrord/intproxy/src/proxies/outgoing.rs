use std::{collections::HashMap, io};

use mirrord_protocol::{
    outgoing::{tcp::DaemonTcpOutgoing, udp::DaemonUdpOutgoing, DaemonConnect, DaemonRead},
    RemoteResult, ResponseError,
};
use thiserror::Error;

use self::interceptor::{Interceptor, InterceptorId};
use crate::{
    background_tasks::{BackgroundTask, BackgroundTasks, MessageBus, TaskSender, TaskUpdate},
    protocol::{
        LocalMessage, MessageId, NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse,
        ProxyToLayerMessage,
    },
    request_queue::{RequestQueue, RequestQueueEmpty},
    ProxyMessage,
};

mod interceptor;
mod protocols;

#[derive(Error, Debug)]
pub enum OutgoingProxyError {
    #[error("agent error: {0}")]
    ResponseError(#[from] ResponseError),
    #[error("failed to match connect response: {0}")]
    RequestQueueEmpty(#[from] RequestQueueEmpty),
    #[error("failed to prepare local listener: {0}")]
    ListenerSetupError(#[from] io::Error),
}

pub struct OutgoingProxy {
    datagrams_reqs: RequestQueue,
    stream_reqs: RequestQueue,
    txs: HashMap<InterceptorId, TaskSender<Interceptor>>,
    background_tasks: BackgroundTasks<InterceptorId, Vec<u8>, io::Error>,
}

impl Default for OutgoingProxy {
    fn default() -> Self {
        Self {
            datagrams_reqs: Default::default(),
            stream_reqs: Default::default(),
            txs: Default::default(),
            background_tasks: Default::default(),
        }
    }
}

impl OutgoingProxy {
    const CHANNEL_SIZE: usize = 512;

    fn queue(&mut self, protocol: NetProtocol) -> &mut RequestQueue {
        match protocol {
            NetProtocol::Datagrams => &mut self.datagrams_reqs,
            NetProtocol::Stream => &mut self.stream_reqs,
        }
    }

    async fn handle_agent_read(
        &mut self,
        read: RemoteResult<DaemonRead>,
        protocol: NetProtocol,
    ) -> Result<(), OutgoingProxyError> {
        let DaemonRead {
            connection_id,
            bytes,
        } = read?;

        let id = InterceptorId {
            connection_id,
            protocol,
        };

        let Some(interceptor) = self.txs.get(&id) else {
            tracing::trace!("{id} does not exist");
            return Ok(());
        };

        interceptor.send(bytes).await;

        Ok(())
    }

    async fn handle_connect_response(
        &mut self,
        connect: RemoteResult<DaemonConnect>,
        protocol: NetProtocol,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), OutgoingProxyError> {
        let message_id = self.queue(protocol).get()?;

        let connect = match connect {
            Ok(connect) => connect,
            Err(e) => {
                message_bus
                    .send(ProxyMessage::ToLayer(LocalMessage {
                        message_id,
                        inner: ProxyToLayerMessage::OutgoingConnect(Err(e)),
                    }))
                    .await;
                return Ok(());
            }
        };

        let DaemonConnect {
            connection_id,
            remote_address,
            local_address,
        } = connect;

        let prepared_socket = protocol.prepare_socket(remote_address).await?;
        let layer_address = prepared_socket.local_address()?;

        let id = InterceptorId {
            connection_id,
            protocol,
        };

        let interceptor = self.background_tasks.register(
            Interceptor::new(prepared_socket),
            id,
            Self::CHANNEL_SIZE,
        );
        self.txs.insert(id, interceptor);

        message_bus
            .send(ProxyMessage::ToLayer(LocalMessage {
                message_id,
                inner: ProxyToLayerMessage::OutgoingConnect(Ok(OutgoingConnectResponse {
                    layer_address,
                    in_cluster_address: local_address,
                })),
            }))
            .await;

        Ok(())
    }

    async fn handle_connect_request(
        &mut self,
        message_id: MessageId,
        request: OutgoingConnectRequest,
        message_bus: &mut MessageBus<Self>,
    ) {
        self.queue(request.protocol).insert(message_id);

        let msg = request.protocol.wrap_agent_connect(request.remote_address);
        message_bus.send(ProxyMessage::ToAgent(msg)).await;
    }
}

pub enum OutgoingProxyMessage {
    AgentStream(DaemonTcpOutgoing),
    AgentDatagrams(DaemonUdpOutgoing),
    LayerConnect(OutgoingConnectRequest, MessageId),
}

impl BackgroundTask for OutgoingProxy {
    type Error = OutgoingProxyError;
    type MessageIn = OutgoingProxyMessage;
    type MessageOut = ProxyMessage;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                msg = message_bus.recv() => match msg {
                    None => break Ok(()),
                    Some(OutgoingProxyMessage::AgentStream(req)) => match req {
                        DaemonTcpOutgoing::Close(close) => {
                            let id = InterceptorId { connection_id: close, protocol: NetProtocol::Stream};
                            self.txs.remove(&id);
                        },
                        DaemonTcpOutgoing::Read(read) => self.handle_agent_read(read, NetProtocol::Stream).await?,
                        DaemonTcpOutgoing::Connect(connect) => self.handle_connect_response(connect, NetProtocol::Stream, message_bus).await?,
                    }
                    Some(OutgoingProxyMessage::AgentDatagrams(req)) => match req {
                        DaemonUdpOutgoing::Close(close) => {
                            let id = InterceptorId { connection_id: close, protocol: NetProtocol::Datagrams};
                            self.txs.remove(&id);
                        }
                        DaemonUdpOutgoing::Read(read) => self.handle_agent_read(read, NetProtocol::Datagrams).await?,
                        DaemonUdpOutgoing::Connect(connect) => self.handle_connect_response(connect, NetProtocol::Datagrams, message_bus).await?,
                    }
                    Some(OutgoingProxyMessage::LayerConnect(req, id)) => self.handle_connect_request(id, req, message_bus).await,
                },

                task_update = self.background_tasks.next(), if !self.background_tasks.is_empty() => match task_update {
                    (id, TaskUpdate::Message(bytes)) => {
                        let msg = id.protocol.wrap_agent_write(id.connection_id, bytes);
                        message_bus.send(ProxyMessage::ToAgent(msg)).await;
                    }
                    (id, TaskUpdate::Finished(res)) => {
                        tracing::trace!("{id} finished: {res:?}");

                        if self.txs.remove(&id).is_some() {
                            let msg = id.protocol.wrap_agent_close(id.connection_id);
                            let _ = message_bus.send(ProxyMessage::ToAgent(msg)).await;
                            self.txs.remove(&id);
                        }
                    }
                },
            }
        }
    }
}
