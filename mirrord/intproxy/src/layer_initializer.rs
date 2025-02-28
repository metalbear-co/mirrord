use std::{io, net::SocketAddr};

use mirrord_intproxy_protocol::{
    codec::{AsyncDecoder, AsyncEncoder, CodecError},
    LayerId, LayerToProxyMessage, LocalMessage, NewSessionRequest, ProxyToLayerMessage,
};
use thiserror::Error;
use tokio::net::{TcpListener, TcpStream};
use tracing::Level;

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    main_tasks::NewLayer,
    ProxyMessage,
};

#[derive(Error, Debug)]
pub enum LayerInitializerError {
    #[error("failed to accept a layer connection: {0}")]
    Accept(io::Error),
    #[error("{0}")]
    Codec(#[from] CodecError),
    #[error("layer did not send any message")]
    NoMessage,
    #[error("layer sent unexpected message: {0:?}")]
    UnexpectedMessage(LayerToProxyMessage),
}

/// Handles logic for accepting new layer connections.
/// Run as a [`BackgroundTask`].
#[derive(Debug)]
pub struct LayerInitializer {
    listener: TcpListener,
    next_layer_id: LayerId,
}

impl LayerInitializer {
    pub fn new(listener: TcpListener) -> Self {
        Self {
            listener,
            next_layer_id: LayerId(0),
        }
    }

    /// Initialize connection with the new layer, assigning a fresh [`LayerId`].
    #[tracing::instrument(level = Level::INFO, skip(stream), ret, err)]
    async fn handle_new_stream(
        &mut self,
        stream: TcpStream,
        layer_address: SocketAddr,
    ) -> Result<NewLayer, LayerInitializerError> {
        let mut decoder: AsyncDecoder<LocalMessage<LayerToProxyMessage>, _> =
            AsyncDecoder::new(stream);
        let msg = decoder
            .receive()
            .await?
            .ok_or(LayerInitializerError::NoMessage)?;

        let id = self.next_layer_id;
        self.next_layer_id.0 += 1;

        let parent_id = match msg.inner {
            LayerToProxyMessage::NewSession(NewSessionRequest::New(process_info)) => {
                tracing::info!(?process_info, "New session");
                None
            }
            LayerToProxyMessage::NewSession(NewSessionRequest::Forked(parent)) => {
                tracing::info!(?parent, "Forked session");
                Some(parent)
            }
            other => return Err(LayerInitializerError::UnexpectedMessage(other)),
        };

        let mut encoder: AsyncEncoder<LocalMessage<ProxyToLayerMessage>, _> =
            AsyncEncoder::new(decoder.into_inner());
        encoder
            .send(&LocalMessage {
                message_id: msg.message_id,
                inner: ProxyToLayerMessage::NewSession(id),
            })
            .await?;
        encoder.flush().await?;

        let stream = encoder.into_inner();

        Ok(NewLayer {
            stream,
            id,
            parent_id,
        })
    }
}

impl BackgroundTask for LayerInitializer {
    type Error = LayerInitializerError;
    type MessageIn = ();
    type MessageOut = ProxyMessage;

    #[tracing::instrument(level = Level::INFO, name = "layer_initializer_main_loop", skip_all, ret, err)]
    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                None = message_bus.recv() => {
                    tracing::debug!("Message bus closed, exiting");
                    break Ok(())
                },

                res = self.listener.accept() => {
                    let (stream, layer_address) = res.map_err(LayerInitializerError::Accept)?;
                    let new_layer = self.handle_new_stream(stream, layer_address).await?;
                    message_bus.send(new_layer).await;
                },
            }
        }
    }
}
