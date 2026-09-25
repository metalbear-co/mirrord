use std::{io, net::SocketAddr};

#[cfg(test)]
use std::sync::Arc;

use futures::{SinkExt, TryStreamExt};
use mirrord_intproxy_protocol::{
    LayerId, LayerToProxyMessage, LocalMessage, NewSessionRequest, ProxyToLayerMessage,
    codec::{AsyncDecoder, AsyncEncoder, CodecError},
};
use thiserror::Error;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::{JoinError, JoinSet},
};
use tokio_util::sync::CancellationToken;
use tracing::Level;

#[cfg(test)]
mod test_support;
#[cfg(test)]
pub(crate) use test_support::{RegistrationGate, RegistrationGateControl};

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
    #[error("layer initialization task failed: {0}")]
    Join(#[from] JoinError),
}

/// Controls the initializer independently of its bounded task message channel.
///
/// Shutdown acknowledgement cannot share the registration channel: registrations already in
/// flight may fill that channel precisely while the owner needs to wait for quiescence.
pub(crate) struct LayerInitializerShutdown {
    pub(crate) cancellation: CancellationToken,
    pub(crate) quiesced: oneshot::Receiver<()>,
    #[cfg(test)]
    pub(crate) registration_gate: Arc<RegistrationGate>,
}

#[derive(Debug)]
struct InitializedLayer {
    layer: NewLayer,
    response_error: Option<CodecError>,
}

/// Handles logic for accepting new layer connections.
/// Run as a [`BackgroundTask`].
#[derive(Debug)]
pub struct LayerInitializer {
    listener: TcpListener,
    next_layer_id: LayerId,
    shutdown: CancellationToken,
    quiesced: Option<oneshot::Sender<()>>,
    #[cfg(test)]
    registration_gate: Arc<RegistrationGate>,
}

impl LayerInitializer {
    pub fn new(listener: TcpListener) -> (Self, LayerInitializerShutdown) {
        let shutdown = CancellationToken::new();
        let (quiesced_tx, quiesced_rx) = oneshot::channel();
        #[cfg(test)]
        let registration_gate = Arc::new(RegistrationGate::new());

        (
            Self {
                listener,
                next_layer_id: LayerId(0),
                shutdown: shutdown.clone(),
                quiesced: Some(quiesced_tx),
                #[cfg(test)]
                registration_gate: registration_gate.clone(),
            },
            LayerInitializerShutdown {
                cancellation: shutdown,
                quiesced: quiesced_rx,
                #[cfg(test)]
                registration_gate,
            },
        )
    }

    /// Initializes one accepted connection.
    ///
    /// Cancellation may discard a connection only while its registration is still undecoded.
    /// Once the process information is known, the result retains the socket and PID even when
    /// sending the handshake response fails, so shutdown can still account for that process.
    #[tracing::instrument(level = Level::INFO, skip(stream, shutdown, registration_gate), ret, err)]
    async fn handle_new_stream(
        stream: TcpStream,
        layer_address: SocketAddr,
        id: LayerId,
        shutdown: CancellationToken,
        #[cfg(test)] registration_gate: Arc<RegistrationGate>,
    ) -> Result<Option<InitializedLayer>, LayerInitializerError> {
        let mut decoder: AsyncDecoder<LocalMessage<LayerToProxyMessage>, _> =
            AsyncDecoder::new(stream);
        let msg = tokio::select! {
            biased;
            msg = decoder.try_next() => msg?.ok_or(LayerInitializerError::NoMessage)?,
            _ = shutdown.cancelled() => return Ok(None),
        };

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
        let response_error = encoder
            .send(LocalMessage {
                message_id: msg.message_id,
                inner: ProxyToLayerMessage::NewSession(id),
            })
            .await
            .err();

        #[cfg(test)]
        registration_gate.pause_after_decode().await;

        Ok(Some(InitializedLayer {
            layer: NewLayer {
                stream: encoder.into_inner(),
                id,
                parent_id: parent_layer,
                process_info,
            },
            response_error,
        }))
    }
}

impl BackgroundTask for LayerInitializer {
    type Error = LayerInitializerError;
    type MessageIn = ();
    type MessageOut = ProxyMessage;

    #[tracing::instrument(level = Level::INFO, name = "layer_initializer_main_loop", skip_all, ret, err)]
    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let mut accepted = JoinSet::new();
        let mut accepting = true;
        let mut first_error = None;

        while accepting {
            tokio::select! {
                biased;
                _ = self.shutdown.cancelled() => {
                    accepting = false;
                },
                None = message_bus.recv() => {
                    tracing::debug!("Message bus closed, exiting");
                    self.shutdown.cancel();
                    accepting = false;
                },
                Some(result) = accepted.join_next(), if !accepted.is_empty() => {
                    Self::finish_initialization(result, message_bus, &mut first_error).await;
                    if first_error.is_some() {
                        self.shutdown.cancel();
                        accepting = false;
                    }
                },
                result = self.listener.accept() => {
                    match result {
                        Ok((stream, layer_address)) => {
                            // Layer requests are small and strictly request-response, so Nagle's
                            // algorithm only adds latency to every hooked libc call.
                            if let Err(error) = stream.set_nodelay(true) {
                                tracing::warn!(%error, %layer_address, "Failed to set TCP_NODELAY on a layer connection");
                            }

                            let id = self.next_layer_id;
                            self.next_layer_id.0 += 1;
                            let shutdown = self.shutdown.clone();
                            #[cfg(test)]
                            let registration_gate = self.registration_gate.clone();
                            accepted.spawn(Self::handle_new_stream(
                                stream,
                                layer_address,
                                id,
                                shutdown,
                                #[cfg(test)]
                                registration_gate,
                            ));
                        }
                        Err(error) => {
                            first_error = Some(LayerInitializerError::Accept(error));
                            self.shutdown.cancel();
                            accepting = false;
                        }
                    }
                },
            }
        }

        while let Some(result) = accepted.join_next().await {
            Self::finish_initialization(result, message_bus, &mut first_error).await;
        }

        if let Some(quiesced) = self.quiesced.take() {
            let _ = quiesced.send(());
        }

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl LayerInitializer {
    async fn finish_initialization(
        result: Result<Result<Option<InitializedLayer>, LayerInitializerError>, JoinError>,
        message_bus: &MessageBus<Self>,
        first_error: &mut Option<LayerInitializerError>,
    ) {
        match result {
            Ok(Ok(Some(initialized))) => {
                message_bus.send(initialized.layer).await;
                if let Some(error) = initialized.response_error {
                    first_error.get_or_insert(LayerInitializerError::Codec(error));
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                first_error.get_or_insert(error);
            }
            Err(error) => {
                first_error.get_or_insert(LayerInitializerError::Join(error));
            }
        }
    }
}
