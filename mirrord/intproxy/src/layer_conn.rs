use tokio::net::{
    tcp::{OwnedReadHalf, OwnedWriteHalf},
    TcpStream,
};

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    codec::{self, AsyncReceiver, AsyncSender, CodecError},
    protocol::{LayerToProxyMessage, LocalMessage, ProxyToLayerMessage},
    ProxyMessage,
};

pub struct LayerConnection {
    layer_codec_tx: AsyncSender<LocalMessage<ProxyToLayerMessage>, OwnedWriteHalf>,
    layer_codec_rx: AsyncReceiver<LocalMessage<LayerToProxyMessage>, OwnedReadHalf>,
}

impl LayerConnection {
    pub fn new(stream: TcpStream) -> Self {
        let (layer_codec_tx, layer_codec_rx) = codec::make_async_framed(stream);

        Self {
            layer_codec_rx,
            layer_codec_tx,
        }
    }
}

impl BackgroundTask for LayerConnection {
    type Error = CodecError;
    type MessageIn = LocalMessage<ProxyToLayerMessage>;
    type MessageOut = ProxyMessage;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), CodecError> {
        loop {
            tokio::select! {
                res = self.layer_codec_rx.receive() => {
                    let msg = res?;
                    match msg {
                        Some(msg) => message_bus.send(ProxyMessage::FromLayer(msg)).await,
                        None => break Ok(()),
                    }
                },

                msg = message_bus.recv() => match msg {
                    Some(msg) => {
                        self.layer_codec_tx.send(&msg).await?;
                    },
                    None => break Ok(()),
                },
            }
        }
    }
}
