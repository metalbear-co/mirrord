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
    file_ops_reqs: RequestQueue,
    get_addr_info_reqs: RequestQueue,
}

pub enum SimpleProxyIn {
    FileReq(LocalMessage<FileRequest>),
    AddrInfoReq(LocalMessage<GetAddrInfoRequest>),
    FileRes(FileResponse),
    AddrInfoRes(GetAddrInfoResponse),
}

pub enum SimpleProxyOut {
    ToAgent(ClientMessage),
    ToLayer(LocalMessage<ProxyToLayerMessage>),
}

impl Task for SimpleProxy {
    type Error = RequestQueueEmpty;
    type Id = &'static str;
    type MessageIn = SimpleProxyIn;
    type MessageOut = SimpleProxyOut;

    fn id(&self) -> Self::Id {
        "SIMPLE_PROXY"
    }

    async fn run(mut self, messages: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        while let Some(msg) = messages.recv().await {
            let out_msg = match msg {
                SimpleProxyIn::AddrInfoRes(res) => {
                    let message_id = self.get_addr_info_reqs.get()?;
                    SimpleProxyOut::ToLayer(LocalMessage {
                        message_id,
                        inner: ProxyToLayerMessage::GetAddrInfo(res),
                    })
                }
                SimpleProxyIn::FileRes(res) => {
                    let message_id = self.file_ops_reqs.get()?;
                    SimpleProxyOut::ToLayer(LocalMessage {
                        message_id,
                        inner: ProxyToLayerMessage::File(res),
                    })
                }
                SimpleProxyIn::AddrInfoReq(req) => {
                    self.get_addr_info_reqs.insert(req.message_id);
                    SimpleProxyOut::ToAgent(ClientMessage::GetAddrInfoRequest(req.inner))
                }
                SimpleProxyIn::FileReq(req) => {
                    self.file_ops_reqs.insert(req.message_id);
                    SimpleProxyOut::ToAgent(ClientMessage::FileRequest(req.inner))
                }
            };

            messages.send(out_msg).await;
        }

        Ok(())
    }
}
