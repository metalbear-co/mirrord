use std::io;

use mirrord_intproxy_protocol::{
    codec::{AsyncDecoder, AsyncEncoder, CodecError},
    ExecveRequest, LayerId, LayerToProxyMessage, LocalMessage, NewSessionRequest,
    ProxyToLayerMessage,
};
use thiserror::Error;
use tokio::net::{TcpListener, TcpStream};
use tracing::info;

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    main_tasks::NewLayer,
    ProxyMessage,
};

#[derive(Error, Debug)]
pub enum LayerInitializerError {
    #[error("failed to accept layer connection: {0}")]
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

    /// Initialize connection with the new layer, assigning fresh [`LayerId`].
    #[tracing::instrument(level = "trace" ret)]
    async fn handle_new_stream(
        &mut self,
        stream: TcpStream,
    ) -> Result<NewLayer, LayerInitializerError> {
        let mut decoder: AsyncDecoder<LocalMessage<LayerToProxyMessage>, _> =
            AsyncDecoder::new(stream);
        let msg = decoder
            .receive()
            .await?
            .ok_or(LayerInitializerError::NoMessage)?;

        let id = self.next_layer_id;
        self.next_layer_id.0 += 1;

        // TODO(alex) [high]: Deal with the sockets we received from the layer!
        tracing::info!("new stream message: {:?}", msg.inner);
        let (parent_id, sockets) = match msg.inner {
            LayerToProxyMessage::NewSession(NewSessionRequest::New(process_info)) => {
                // TODO(alex) [high]: Looks like the socket is created in another process,
                // and we end up here, not in the forked match!
                info!(?process_info, "new session");
                (None, Default::default())
            }
            LayerToProxyMessage::NewSession(NewSessionRequest::Forked(parent)) => {
                (Some(parent), Default::default())
            }
            LayerToProxyMessage::NewSession(NewSessionRequest::Execve(ExecveRequest {
                parent,
                shared_sockets,
            })) => (Some(parent), shared_sockets),

            other => return Err(LayerInitializerError::UnexpectedMessage(other)),
        };

        tracing::info!("lesockets {sockets:?} parent id {parent_id:?}");

        let mut encoder: AsyncEncoder<LocalMessage<ProxyToLayerMessage>, _> =
            AsyncEncoder::new(decoder.into_inner());
        encoder
            .send(&LocalMessage {
                message_id: msg.message_id,
                inner: ProxyToLayerMessage::NewSession(id, sockets),
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

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                None = message_bus.recv() => {
                    tracing::trace!("message bus closed, exiting");
                    break Ok(())
                },

                res = self.listener.accept() => {
                    let (stream, peer) = res.map_err(LayerInitializerError::Accept)?;
                    match self.handle_new_stream(stream).await {
                        Ok(new_layer) => message_bus.send(new_layer).await,
                        Err(e) => {
                            tracing::error!("failed to initialize connection with peer {peer}: {e}");
                            break Err(e)
                        }
                    }
                },
            }
        }
    }
}
