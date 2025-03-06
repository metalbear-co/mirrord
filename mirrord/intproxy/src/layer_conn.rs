//! Implementation of `layer <-> proxy` connection through a [`TcpStream`].

use mirrord_intproxy_protocol::{
    codec::{self, AsyncDecoder, AsyncEncoder, CodecError},
    LayerId, LayerToProxyMessage, LocalMessage, ProxyToLayerMessage,
};
use tokio::net::{
    tcp::{OwnedReadHalf, OwnedWriteHalf},
    TcpStream,
};
use tracing::Level;

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    main_tasks::FromLayer,
    ProxyMessage,
};

/// Handles logic of a single `layer <-> proxy` connection.
/// Run as a [`BackgroundTask`].
pub struct LayerConnection {
    layer_codec_tx: AsyncEncoder<LocalMessage<ProxyToLayerMessage>, OwnedWriteHalf>,
    layer_codec_rx: AsyncDecoder<LocalMessage<LayerToProxyMessage>, OwnedReadHalf>,
    layer_id: LayerId,
}

impl LayerConnection {
    /// Wraps a raw [`TcpStream`] to be used as a `layer <-> proxy` connection.
    pub fn new(stream: TcpStream, layer_id: LayerId) -> Self {
        let (layer_codec_tx, layer_codec_rx) = codec::make_async_framed(stream);

        Self {
            layer_codec_rx,
            layer_codec_tx,
            layer_id,
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err(level = Level::TRACE))]
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

    #[tracing::instrument(
        level = Level::INFO, name = "layer_connection_main_loop",
        skip_all, fields(layer_id = ?self.layer_id),
        ret, err,
    )]
    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), CodecError> {
        loop {
            tokio::select! {
                res = self.layer_codec_rx.receive() => match res {
                    Err(e) => {
                        break Err(e);
                    },
                    Ok(None) => {
                        tracing::debug!("Layer closed connection, exiting");
                        break Ok(());
                    }
                    Ok(Some(msg)) => message_bus.send(FromLayer { message: msg.inner, message_id: msg.message_id, layer_id: self.layer_id }).await,
                },

                msg = message_bus.recv() => match msg {
                    Some(msg) => self.send_and_flush(&msg).await?,
                    None => {
                        tracing::debug!("Message bus closed, exiting");
                        break Ok(());
                    },
                },
            }
        }
    }
}
