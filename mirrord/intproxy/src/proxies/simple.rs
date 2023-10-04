use mirrord_protocol::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    ClientMessage, FileRequest, FileResponse,
};

use crate::{
    protocol::{LocalMessage, ProxyToLayerMessage},
    request_queue::{RequestQueue, RequestQueueEmpty},
    task_manager::{MessageBus, Task},
};

/// For passing messages between the layer and the agent without custom internal logic.
#[derive(Default)]
pub struct SimpleProxy {
    file_ops: RequestQueue,
    get_addr_info: RequestQueue,
}

pub enum SimpleProxyMessageIn {
    FileReq(LocalMessage<FileRequest>),
    AddrInfoReq(LocalMessage<GetAddrInfoRequest>),
    FileRes(FileResponse),
    AddrInfoRes(GetAddrInfoResponse),
}

pub enum SimpleProxyMessageOut {
    ToAgent(ClientMessage),
    ToLayer(LocalMessage<ProxyToLayerMessage>),
}

impl Task for SimpleProxy {
    type Error = RequestQueueEmpty;
    type Id = &'static str;
    type MessageIn = SimpleProxyMessageIn;
    type MessageOut = SimpleProxyMessageOut;

    fn id(&self) -> Self::Id {
        "SIMPLE_PROXY"
    }

    async fn run(mut self, messages: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        while let Some(msg) = messages.recv().await {
            let out_msg = match msg {
                SimpleProxyMessageIn::AddrInfoRes(res) => {
                    let message_id = self.get_addr_info.get()?;
                    SimpleProxyMessageOut::ToLayer(LocalMessage {
                        message_id,
                        inner: ProxyToLayerMessage::GetAddrInfo(res),
                    })
                }
                SimpleProxyMessageIn::FileRes(res) => {
                    let message_id = self.file_ops.get()?;
                    SimpleProxyMessageOut::ToLayer(LocalMessage {
                        message_id,
                        inner: ProxyToLayerMessage::File(res),
                    })
                }
                SimpleProxyMessageIn::AddrInfoReq(req) => {
                    self.get_addr_info.insert(req.message_id);
                    SimpleProxyMessageOut::ToAgent(ClientMessage::GetAddrInfoRequest(req.inner))
                }
                SimpleProxyMessageIn::FileReq(req) => {
                    self.get_addr_info.insert(req.message_id);
                    SimpleProxyMessageOut::ToAgent(ClientMessage::FileRequest(req.inner))
                }
            };

            messages.send(out_msg).await;
        }

        Ok(())
    }
}
