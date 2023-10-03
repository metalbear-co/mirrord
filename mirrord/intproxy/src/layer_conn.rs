use tokio::{
    net::TcpStream,
    sync::mpsc::{self, Receiver, Sender},
};

use crate::{
    codec::{self, CodecError},
    protocol::{LayerToProxyMessage, LocalMessage, ProxyToLayerMessage},
    system::Component,
};

pub struct LayerConnector {
    stream: TcpStream,
    incoming_tx: Sender<LocalMessage<LayerToProxyMessage>>,
}

impl LayerConnector {
    pub fn new(stream: TcpStream) -> (Self, Receiver<LocalMessage<LayerToProxyMessage>>) {
        let (incoming_tx, incoming_rx) = mpsc::channel(512);

        (
            Self {
                stream,
                incoming_tx,
            },
            incoming_rx,
        )
    }
}

impl Component for LayerConnector {
    type Message = LocalMessage<ProxyToLayerMessage>;
    type Error = CodecError;
    type Id = &'static str;

    fn id(&self) -> Self::Id {
        "LAYER_CONNECTOR"
    }

    async fn run(self, mut message_rx: Receiver<Self::Message>) -> Result<(), Self::Error> {
        let (mut layer_sender, mut layer_receiver) = codec::make_async_framed::<
            LocalMessage<ProxyToLayerMessage>,
            LocalMessage<LayerToProxyMessage>,
        >(self.stream);

        loop {
            tokio::select! {
                msg = message_rx.recv() => match msg {
                    None => break Ok(()),
                    Some(msg) => layer_sender.send(&msg).await?,
                },
                res = layer_receiver.receive() => match res? {
                    Some(msg) => {
                        if self.incoming_tx.send(msg).await.is_err() {
                            break Ok(());
                        }
                    }
                    None => break Ok(()),
                },
            }
        }
    }
}
