use std::{collections::HashMap, io};

use futures::StreamExt;
use mirrord_protocol::{
    outgoing::{tcp::DaemonTcpOutgoing, udp::DaemonUdpOutgoing, DaemonConnect, DaemonRead},
    RemoteResult, ResponseError,
};
use thiserror::Error;
use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    task::JoinHandle,
};
use tokio_stream::{wrappers::ReceiverStream, StreamMap, StreamNotifyClose};

use super::interceptor::{Interceptor, InterceptorId};
use crate::{
    error::BackgroundTaskDown,
    protocol::{
        LocalMessage, MessageId, NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse,
        ProxyToLayerMessage,
    },
    request_queue::{RequestQueue, RequestQueueEmpty},
    ProxyMessage,
};

#[derive(Error, Debug)]
pub enum OutgoingProxyError {
    #[error("background task panicked")]
    Panic,
    #[error("agent error: {0}")]
    ResponseError(#[from] ResponseError),
    #[error("failed to match connect response: {0}")]
    RequestQueueEmpty(#[from] RequestQueueEmpty),
    #[error("failed to prepare local listener: {0}")]
    ListenerSetupError(#[from] io::Error),
    #[error("{0} already exists")]
    InterceptorExists(InterceptorId),
}

struct BackgroundTask {
    datagrams_reqs: RequestQueue,
    stream_reqs: RequestQueue,
    interceptors: HashMap<InterceptorId, Interceptor>,
    streams: StreamMap<InterceptorId, StreamNotifyClose<ReceiverStream<Vec<u8>>>>,
    tx: Sender<ProxyMessage>,
    rx: Receiver<BackgroundTaskRequest>,
}

impl BackgroundTask {
    const CHANNEL_SIZE: usize = 512;

    async fn run(mut self) -> Result<(), OutgoingProxyError> {
        loop {
            tokio::select! {
                _ = self.tx.closed() => break Ok(()),

                msg = self.rx.recv() => match msg {
                    Some(BackgroundTaskRequest::AgentStream(req)) => match req {
                        DaemonTcpOutgoing::Close(close) => {
                            let id = InterceptorId { connection_id: close, protocol: NetProtocol::Stream};
                            self.remove_interceptor(&id).await;
                        },
                        DaemonTcpOutgoing::Read(read) => self.handle_agent_read(read, NetProtocol::Stream).await?,
                        DaemonTcpOutgoing::Connect(connect) => self.handle_connect_response(connect, NetProtocol::Stream).await?,
                    }
                    Some(BackgroundTaskRequest::AgentDatagrams(req)) => match req {
                        DaemonUdpOutgoing::Close(close) => {
                            let id = InterceptorId { connection_id: close, protocol: NetProtocol::Datagrams};
                            self.remove_interceptor(&id).await;
                        }
                        DaemonUdpOutgoing::Read(read) => self.handle_agent_read(read, NetProtocol::Datagrams).await?,
                        DaemonUdpOutgoing::Connect(connect) => self.handle_connect_response(connect, NetProtocol::Datagrams).await?,
                    }
                    Some(BackgroundTaskRequest::LayerConnect(req, id)) => self.handle_connect_request(id, req).await,
                    None => break Ok(()),
                },

                Some(res) = self.streams.next() => match res {
                    (id, None) => {
                        let msg = id.protocol.wrap_agent_close(id.connection_id);
                        let _ = self.tx.send(ProxyMessage::ToAgent(msg)).await;
                        self.remove_interceptor(&id);
                    }
                    (id, Some(bytes)) => {
                        let msg = id.protocol.wrap_agent_write(id.connection_id, bytes);
                        let _ = self.tx.send(ProxyMessage::ToAgent(msg));
                    }
                },
            };
        }
    }

    fn queue(&mut self, protocol: NetProtocol) -> &mut RequestQueue {
        match protocol {
            NetProtocol::Datagrams => &mut self.datagrams_reqs,
            NetProtocol::Stream => &mut self.stream_reqs,
        }
    }

    /// Remove interceptor from maps in this struct and wait for its result.
    async fn remove_interceptor(&mut self, id: &InterceptorId) {
        let Some(interceptor) = self.interceptors.remove(id) else {
            tracing::trace!("{id} does not exist");
            return;
        };

        self.streams.remove(id);

        if let Err(e) = interceptor.result().await {
            tracing::error!("{id} failed: {e:?}");
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

        let Some(interceptor) = self.interceptors.get(&id) else {
            tracing::trace!("{id} does not exist");
            return Ok(());
        };

        let res = interceptor.send_bytes(bytes).await;
        if let Err(e) = res {
            tracing::trace!("failed to send data to {id}: {e}");
            self.remove_interceptor(&id).await;
        }

        Ok(())
    }

    async fn handle_connect_response(
        &mut self,
        connect: RemoteResult<DaemonConnect>,
        protocol: NetProtocol,
    ) -> Result<(), OutgoingProxyError> {
        let message_id = self.queue(protocol).get()?;

        let connect = match connect {
            Ok(connect) => connect,
            Err(e) => {
                let _ = self
                    .tx
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
        let (bytes_tx, bytes_rx) = mpsc::channel(Self::CHANNEL_SIZE);

        let interceptor = Interceptor::new(connection_id, protocol, prepared_socket, bytes_tx);

        if self.interceptors.insert(id, interceptor).is_some() {
            return Err(OutgoingProxyError::InterceptorExists(id));
        }
        self.streams
            .insert(id, StreamNotifyClose::new(ReceiverStream::new(bytes_rx)));

        let _ = self
            .tx
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
    ) {
        self.queue(request.protocol).insert(message_id);

        let msg = request.protocol.wrap_agent_connect(request.remote_address);
        let _ = self.tx.send(ProxyMessage::ToAgent(msg)).await;
    }
}

enum BackgroundTaskRequest {
    AgentStream(DaemonTcpOutgoing),
    AgentDatagrams(DaemonUdpOutgoing),
    LayerConnect(OutgoingConnectRequest, MessageId),
}

pub struct OutgoingProxy {
    task: JoinHandle<Result<(), OutgoingProxyError>>,
    tx: Sender<BackgroundTaskRequest>,
}

impl OutgoingProxy {
    const CHANNEL_SIZE: usize = 512;

    pub fn new(tx: Sender<ProxyMessage>) -> Self {
        let (task_tx, task_rx) = mpsc::channel(Self::CHANNEL_SIZE);

        let task = BackgroundTask {
            datagrams_reqs: Default::default(),
            stream_reqs: Default::default(),
            interceptors: Default::default(),
            streams: Default::default(),
            tx,
            rx: task_rx,
        };

        let task = tokio::spawn(task.run());

        Self { task, tx: task_tx }
    }

    async fn send_request(&self, request: BackgroundTaskRequest) -> Result<(), BackgroundTaskDown> {
        self.tx.send(request).await.map_err(|_| BackgroundTaskDown)
    }

    pub async fn handle_agent_stream_msg(
        &self,
        message: DaemonTcpOutgoing,
    ) -> Result<(), BackgroundTaskDown> {
        self.send_request(BackgroundTaskRequest::AgentStream(message))
            .await
    }

    pub async fn handle_agent_datagrams_msg(
        &self,
        message: DaemonUdpOutgoing,
    ) -> Result<(), BackgroundTaskDown> {
        self.send_request(BackgroundTaskRequest::AgentDatagrams(message))
            .await
    }

    pub async fn handle_layer_connect_req(
        &self,
        request: OutgoingConnectRequest,
        id: MessageId,
    ) -> Result<(), BackgroundTaskDown> {
        self.send_request(BackgroundTaskRequest::LayerConnect(request, id))
            .await
    }

    pub async fn result(self) -> Result<(), OutgoingProxyError> {
        self.task.await.map_err(|_| OutgoingProxyError::Panic)?
    }
}
