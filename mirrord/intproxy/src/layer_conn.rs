use std::panic::AssertUnwindSafe;

use futures::FutureExt;
use thiserror::Error;
use tokio::{
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpStream,
    },
    sync::mpsc::{self, Receiver, Sender},
};

use crate::{
    codec::{self, AsyncReceiver, AsyncSender, CodecError},
    protocol::{LayerToProxyMessage, LocalMessage, ProxyToLayerMessage},
};

#[derive(Error, Debug)]
pub enum LayerCommunicationError {
    #[error("{0}")]
    CodecError(#[from] CodecError),
    #[error("channel closed")]
    ChannelClosed,
}

pub type Result<T> = core::result::Result<T, LayerCommunicationError>;

struct ConnectionTask {
    layer_to_proxy_tx: Sender<LocalMessage<LayerToProxyMessage>>,
    proxy_to_layer_rx: Receiver<LocalMessage<ProxyToLayerMessage>>,
    layer_codec_tx: AsyncSender<LocalMessage<ProxyToLayerMessage>, OwnedWriteHalf>,
    layer_codec_rx: AsyncReceiver<LocalMessage<LayerToProxyMessage>, OwnedReadHalf>,
}

impl ConnectionTask {
    async fn run(mut self) -> Result<()> {
        loop {
            tokio::select! {
                res = self.layer_codec_rx.receive() => {
                    let msg = res?;
                    match msg {
                        Some(msg) => {
                            self.layer_to_proxy_tx.send(msg).await.map_err(|_| LayerCommunicationError::ChannelClosed)?;
                        }
                        None => {
                            tracing::trace!("no more messages from the layer, shutting down connection task");
                            break Ok(());
                        }
                    }
                },

                msg = self.proxy_to_layer_rx.recv() => match msg {
                    Some(msg) => {
                        self.layer_codec_tx.send(&msg).await?;
                    },
                    None => break Err(LayerCommunicationError::ChannelClosed),
                }
            }
        }
    }
}

pub struct LayerConnection {
    layer_sender: LayerSender,
    layer_rx: Receiver<LocalMessage<LayerToProxyMessage>>,
}

impl LayerConnection {
    pub fn new(stream: TcpStream, channel_size: usize) -> Self {
        let (layer_codec_tx, layer_codec_rx) = codec::make_async_framed::<
            LocalMessage<ProxyToLayerMessage>,
            LocalMessage<LayerToProxyMessage>,
        >(stream);

        let (proxy_to_layer_tx, proxy_to_layer_rx) = mpsc::channel(channel_size);
        let (layer_to_proxy_tx, layer_to_proxy_rx) = mpsc::channel(channel_size);

        let connection_task = ConnectionTask {
            layer_codec_rx,
            layer_codec_tx,
            layer_to_proxy_tx,
            proxy_to_layer_rx,
        };

        tokio::spawn(async move {
            let res = AssertUnwindSafe(connection_task.run()).catch_unwind().await;

            match res {
                Err(..) => tracing::error!("layer connection task panicked"),
                Ok(Err(err)) => tracing::error!("layer connection task failed: {err}"),
                Ok(Ok(())) => {}
            }
        });

        Self {
            layer_sender: LayerSender(proxy_to_layer_tx),
            layer_rx: layer_to_proxy_rx,
        }
    }

    pub fn sender(&self) -> &LayerSender {
        &self.layer_sender
    }

    pub async fn receive(&mut self) -> Option<LocalMessage<LayerToProxyMessage>> {
        self.layer_rx.recv().await
    }
}

#[derive(Clone)]
pub struct LayerSender(Sender<LocalMessage<ProxyToLayerMessage>>);

impl LayerSender {
    pub async fn send(&self, message: LocalMessage<ProxyToLayerMessage>) -> Result<()> {
        self.0
            .send(message)
            .await
            .map_err(|_| LayerCommunicationError::ChannelClosed)
    }
}
