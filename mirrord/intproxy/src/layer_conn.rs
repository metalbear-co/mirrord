//! Implementation of `layer <-> proxy` connection through a [`TcpStream`].

use tokio::net::{
    tcp::{OwnedReadHalf, OwnedWriteHalf},
    TcpStream,
};

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    codec::{self, AsyncDecoder, AsyncEncoder, CodecError},
    protocol::{LayerToProxyMessage, LocalMessage, ProxyToLayerMessage},
    ProxyMessage,
};

/// Handles logic of the `layer <-> proxy` connection.
/// Run as a [`BackgroundTask`] by each [`ProxySession`](crate::session::ProxySession).
pub struct LayerConnection {
    layer_codec_tx: AsyncEncoder<LocalMessage<ProxyToLayerMessage>, OwnedWriteHalf>,
    layer_codec_rx: AsyncDecoder<LocalMessage<LayerToProxyMessage>, OwnedReadHalf>,
}

impl LayerConnection {
    /// Wraps a raw [`TcpStream`] to be used as a `layer <-> proxy` connection.
    pub fn new(stream: TcpStream) -> Self {
        let (layer_codec_tx, layer_codec_rx) = codec::make_async_framed(stream);

        Self {
            layer_codec_rx,
            layer_codec_tx,
        }
    }

    async fn send_and_flush(
        &mut self,
        msg: &LocalMessage<ProxyToLayerMessage>,
    ) -> Result<(), CodecError> {
        self.layer_codec_tx.send(msg).await?;
        self.layer_codec_tx.flush().await
    }
}

impl BackgroundTask for LayerConnection {
    type Error = CodecError;
    type MessageIn = LocalMessage<ProxyToLayerMessage>;
    type MessageOut = ProxyMessage;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), CodecError> {
        loop {
            tokio::select! {
                res = self.layer_codec_rx.receive() => match res {
                    Err(e) => {
                        tracing::error!("layer connection failed with {e:?} when receiving");
                        break Err(e);
                    },
                    Ok(None) => {
                        tracing::trace!("message bus closed, exiting");
                        break Ok(());
                    }
                    Ok(Some(msg)) => message_bus.send(ProxyMessage::FromLayer(msg)).await,
                },

                msg = message_bus.recv() => match msg {
                    Some(msg) => {
                        if let Err(e) = self.send_and_flush(&msg).await {
                            tracing::error!("layer connection failed with {e:?} when sending {msg:?}");
                            break Err(e);
                        }
                    },
                    None => {
                        tracing::trace!("no more messages from the proxy, exiting");
                        break Ok(());
                    },
                },
            }
        }
    }
}
