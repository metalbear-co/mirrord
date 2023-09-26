use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::mpsc::{self, error::SendError, Receiver, Sender},
    task,
};

use crate::{
    codec::{self, CodecError},
    protocol::{LayerToProxyMessage, LocalMessage, ProxyToLayerMessage},
};

#[derive(Error, Debug)]
pub enum LayerCommunicationFailed {
    #[error("channel is closed")]
    ChannelClosed,
    #[error("binary protocol failed: {0}")]
    CodecFailed(#[from] CodecError),
    #[error("received unexpected message")]
    UnexpectedMessage(LayerToProxyMessage),
}

impl From<SendError<LocalMessage<ProxyToLayerMessage>>> for LayerCommunicationFailed {
    fn from(_value: SendError<LocalMessage<ProxyToLayerMessage>>) -> Self {
        Self::ChannelClosed
    }
}

pub type Result<T> = core::result::Result<T, LayerCommunicationFailed>;

pub struct LayerConnection {
    sender: LayerSender,
    receiver: Receiver<LocalMessage<LayerToProxyMessage>>,
}

impl LayerConnection {
    const CHANNEL_SIZE: usize = 512;

    pub fn new(conn: TcpStream) -> LayerConnection {
        let (mut layer_sender, mut layer_receiver) = codec::make_async_framed::<
            LocalMessage<ProxyToLayerMessage>,
            LocalMessage<LayerToProxyMessage>,
        >(conn);

        let (layer_tx, layer_rx) = mpsc::channel(Self::CHANNEL_SIZE);
        let (proxy_tx, mut proxy_rx) = mpsc::channel(Self::CHANNEL_SIZE);

        task::spawn(async move {
            loop {
                tokio::select! {
                    res = layer_receiver.receive() => {
                        match res {
                            Ok(None) => {
                                tracing::trace!("no more messages from layer to proxy");
                                break;
                            }
                            Ok(Some(message)) => {
                                if let Err(e) = layer_tx.send(message).await {
                                    tracing::trace!("internal layer to proxy channel closed, dropping message {:?}", e.0);
                                    break;
                                }
                            }
                            Err(err) => {
                                tracing::error!("failed to receive message from layer: {err:?}");
                                break;
                            }
                        }
                    }
                    res = proxy_rx.recv() => {
                        let Some(message) = res else {
                            tracing::trace!("no more messages from proxy to layer");
                            break;
                        };

                        if let Err(err) = layer_sender.send(&message).await {
                            tracing::error!("failed to send message to layer: {err:?}");
                            break;
                        }
                    }
                }
            }
        });

        LayerConnection {
            sender: LayerSender(proxy_tx),
            receiver: layer_rx,
        }
    }

    pub fn sender(&self) -> &LayerSender {
        &self.sender
    }

    pub async fn receive(&mut self) -> Option<LocalMessage<LayerToProxyMessage>> {
        self.receiver.recv().await
    }
}

#[derive(Clone)]
pub struct LayerSender(Sender<LocalMessage<ProxyToLayerMessage>>);

impl LayerSender {
    pub async fn send(&self, message: LocalMessage<ProxyToLayerMessage>) -> Result<()> {
        self.0.send(message).await?;
        Ok(())
    }
}
