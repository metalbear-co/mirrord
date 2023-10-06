use mirrord_protocol::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    ClientMessage, FileRequest, FileResponse,
};
use thiserror::Error;
use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    task::JoinHandle,
};

use crate::{
    error::BackgroundTaskDown,
    protocol::{LocalMessage, MessageId, ProxyToLayerMessage},
    request_queue::{RequestQueue, RequestQueueEmpty},
    ProxyMessage,
};

#[derive(Error, Debug)]
pub enum SimpleProxyError {
    #[error("background task panicked")]
    Panic,
    #[error("{0}")]
    RequestQueueEmpty(#[from] RequestQueueEmpty),
}

enum TaskMessage {
    FileReq(MessageId, FileRequest),
    FileRes(FileResponse),
    AddrInfoReq(MessageId, GetAddrInfoRequest),
    AddrInfoRes(GetAddrInfoResponse),
}

struct BackgroundTask {
    file_reqs: RequestQueue,
    addr_info_reqs: RequestQueue,
    tx: Sender<ProxyMessage>,
    rx: Receiver<TaskMessage>,
}

impl BackgroundTask {
    async fn run(mut self) -> Result<(), SimpleProxyError> {
        loop {
            tokio::select! {
                _ = self.tx.closed() => break Ok(()),

                msg = self.rx.recv() => match msg {
                    None => break Ok(()),
                    Some(TaskMessage::FileReq(id, req)) => {
                        self.file_reqs.insert(id);
                        let _ = self.tx.send(ProxyMessage::ToAgent(ClientMessage::FileRequest(req))).await;
                    }
                    Some(TaskMessage::FileRes(res)) => {
                        let message_id = self.file_reqs.get()?;
                        let _ = self.tx.send(ProxyMessage::ToLayer(LocalMessage { message_id, inner: ProxyToLayerMessage::File(res) })).await;
                    }
                    Some(TaskMessage::AddrInfoReq(id, req)) => {
                        self.addr_info_reqs.insert(id);
                        let _ = self.tx.send(ProxyMessage::ToAgent(ClientMessage::GetAddrInfoRequest(req))).await;
                    }
                    Some(TaskMessage::AddrInfoRes(res)) => {
                        let message_id = self.addr_info_reqs.get()?;
                        let _ = self.tx.send(ProxyMessage::ToLayer(LocalMessage { message_id, inner: ProxyToLayerMessage::GetAddrInfo(res)})).await;
                    }
                }
            }
        }
    }
}

/// For passing messages between the layer and the agent without custom internal logic.
pub struct SimpleProxy {
    task: JoinHandle<Result<(), SimpleProxyError>>,
    tx: Sender<TaskMessage>,
}

impl SimpleProxy {
    const CHANNEL_SIZE: usize = 512;

    pub fn new(tx: Sender<ProxyMessage>) -> Self {
        let (task_tx, task_rx) = mpsc::channel(Self::CHANNEL_SIZE);

        let task = BackgroundTask {
            file_reqs: Default::default(),
            addr_info_reqs: Default::default(),
            tx,
            rx: task_rx,
        };

        let task = tokio::spawn(task.run());

        Self { task, tx: task_tx }
    }

    async fn send_to_task(&self, msg: TaskMessage) -> Result<(), BackgroundTaskDown> {
        self.tx.send(msg).await.map_err(|_| BackgroundTaskDown)
    }

    pub async fn handle_file_req(
        &mut self,
        req: FileRequest,
        id: MessageId,
    ) -> Result<(), BackgroundTaskDown> {
        self.send_to_task(TaskMessage::FileReq(id, req)).await
    }

    pub async fn handle_addr_info_req(
        &mut self,
        req: GetAddrInfoRequest,
        id: MessageId,
    ) -> Result<(), BackgroundTaskDown> {
        self.send_to_task(TaskMessage::AddrInfoReq(id, req)).await
    }

    pub async fn handle_file_res(&mut self, res: FileResponse) -> Result<(), BackgroundTaskDown> {
        self.send_to_task(TaskMessage::FileRes(res)).await
    }

    pub async fn handle_addr_info_res(
        &mut self,
        res: GetAddrInfoResponse,
    ) -> Result<(), BackgroundTaskDown> {
        self.send_to_task(TaskMessage::AddrInfoRes(res)).await
    }

    pub async fn result(self) -> Result<(), SimpleProxyError> {
        self.task.await.map_err(|_| SimpleProxyError::Panic)?
    }
}
