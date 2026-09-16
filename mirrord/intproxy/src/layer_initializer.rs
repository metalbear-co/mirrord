use std::{io, net::SocketAddr};

use futures::{SinkExt, TryStreamExt};
use mirrord_intproxy_protocol::{
    LayerId, LayerToProxyMessage, LocalMessage, NewSessionRequest, ProxyToLayerMessage,
    codec::{AsyncDecoder, AsyncEncoder, CodecError},
};
use thiserror::Error;
use tokio::net::{TcpListener, TcpStream};
use tracing::Level;

use crate::{
    ProxyMessage,
    background_tasks::{BackgroundTask, MessageBus},
    main_tasks::NewLayer,
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
            .try_next()
            .await?
            .ok_or(LayerInitializerError::NoMessage)?;

        let id = self.next_layer_id;
        self.next_layer_id.0 += 1;

        let NewSessionRequest {
            parent_layer,
            process_info,
        } = match msg.inner {
            LayerToProxyMessage::NewSession(request) => request,
            other => return Err(LayerInitializerError::UnexpectedMessage(other)),
        };
        tracing::info!(?parent_layer, ?process_info, "New layer connected");

        let mut encoder: AsyncEncoder<LocalMessage<ProxyToLayerMessage>, _> =
            AsyncEncoder::new(decoder.into_inner());
        encoder
            .send(LocalMessage {
                message_id: msg.message_id,
                inner: ProxyToLayerMessage::NewSession(id),
            })
            .await?;

        let stream = encoder.into_inner();

        Ok(NewLayer {
            stream,
            id,
            parent_id: parent_layer,
            process_info,
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
                    // Layer requests are small and strictly request-response, so Nagle's algorithm
                    // only adds latency to every hooked libc call.
                    if let Err(error) = stream.set_nodelay(true) {
                        tracing::warn!(%error, %layer_address, "Failed to set TCP_NODELAY on a layer connection");
                    }
                    match self.handle_new_stream(stream, layer_address).await {
                        Ok(new_layer) => message_bus.send(new_layer).await,
                        // One layer failing its handshake is not a proxy failure. A short-lived
                        // process exits before it finishes the handshake, which resets its socket,
                        // and a process is always free to exit. Escalating that would fail this
                        // task, put the proxy in the failover state, and terminate every other
                        // injected process in the session. Only `accept` failing is fatal, because
                        // then the listener itself is gone.
                        Err(error) => tracing::warn!(
                            %error,
                            %layer_address,
                            "Failed to initialize a layer connection, dropping it",
                        ),
                    }
                },
            }
        }
    }
}
