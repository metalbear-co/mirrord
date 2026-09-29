use std::{io, net::SocketAddr};

use futures::{SinkExt, TryStreamExt};
use mirrord_intproxy_protocol::{
    LayerId, LayerToProxyMessage, LocalMessage, NewSessionRequest, ProxyToLayerMessage,
    codec::{AsyncDecoder, AsyncEncoder, CodecError},
};
use thiserror::Error;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tokio_util::sync::CancellationToken;
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

/// Controls the initializer independently of its bounded task message channel.
///
/// Shutdown acknowledgement cannot share the registration channel: registrations already in
/// flight may fill that channel precisely while the owner needs to wait for quiescence.
pub(crate) struct LayerInitializerShutdown {
    pub(crate) cancellation: CancellationToken,
    pub(crate) quiesced: oneshot::Receiver<()>,
}

#[derive(Debug)]
struct InitializedLayer {
    layer: NewLayer,
    response_error: Option<CodecError>,
}

/// Handles logic for accepting new layer connections.
///
/// Initializing one connection at a time bounds pending handshakes without spawning a task per
/// socket. Shutdown cancels an undecoded read so a silent client cannot hold up cleanup.
/// Run as a [`BackgroundTask`].
#[derive(Debug)]
pub struct LayerInitializer {
    listener: TcpListener,
    next_layer_id: LayerId,
    shutdown: CancellationToken,
    quiesced: Option<oneshot::Sender<()>>,
}

impl LayerInitializer {
    pub fn new(listener: TcpListener) -> (Self, LayerInitializerShutdown) {
        let shutdown = CancellationToken::new();
        let (quiesced_tx, quiesced_rx) = oneshot::channel();
        (
            Self {
                listener,
                next_layer_id: LayerId(0),
                shutdown: shutdown.clone(),
                quiesced: Some(quiesced_tx),
            },
            LayerInitializerShutdown {
                cancellation: shutdown,
                quiesced: quiesced_rx,
            },
        )
    }

    /// Initializes one accepted connection.
    ///
    /// Cancellation discards an undecoded connection. After decoding, the result retains the PID
    /// even if shutdown interrupts the response, so the owner can account for it before exiting.
    #[tracing::instrument(level = Level::INFO, skip(stream, shutdown), ret, err)]
    async fn handle_new_stream(
        stream: TcpStream,
        layer_address: SocketAddr,
        id: LayerId,
        shutdown: &CancellationToken,
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
        let response_error = tokio::select! {
            biased;
            _ = shutdown.cancelled() => None,
            result = encoder.send(LocalMessage {
                message_id: msg.message_id,
                inner: ProxyToLayerMessage::NewSession(id),
            }) => result.err(),
        };

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
        let result = loop {
            tokio::select! {
                biased;
                _ = self.shutdown.cancelled() => break Ok(()),
                None = message_bus.recv() => {
                    tracing::debug!("Message bus closed, exiting");
                    self.shutdown.cancel();
                    break Ok(());
                },
                result = self.listener.accept() => {
                    let (stream, layer_address) = match result {
                        Ok(accepted) => accepted,
                        Err(error) => {
                            self.shutdown.cancel();
                            break Err(LayerInitializerError::Accept(error));
                        }
                    };
                    // Layer requests are small and strictly request-response, so Nagle's
                    // algorithm only adds latency to every hooked libc call.
                    if let Err(error) = stream.set_nodelay(true) {
                        tracing::warn!(%error, %layer_address, "Failed to set TCP_NODELAY on a layer connection");
                    }

                    let id = self.next_layer_id;
                    self.next_layer_id.0 += 1;
                    match Self::handle_new_stream(stream, layer_address, id, &self.shutdown).await {
                        Ok(Some(initialized)) => {
                            // A decoded PID must cross the output channel before quiescence is
                            // acknowledged, even if the response failed or shutdown arrived.
                            message_bus.send(initialized.layer).await;
                            if let Some(error) = initialized.response_error {
                                tracing::warn!(%error, %layer_address, "Failed to send the handshake response to a layer connection");
                            }
                        }
                        Ok(None) => {}
                        Err(error) => tracing::warn!(
                            %error,
                            %layer_address,
                            "Failed to initialize a layer connection, dropping it",
                        ),
                    }
                },
            }
        };

        if let Some(quiesced) = self.quiesced.take() {
            let _ = quiesced.send(());
        }

        result
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use futures::{SinkExt, StreamExt};
    use mirrord_intproxy_protocol::{
        LayerToProxyMessage, LocalMessage, NewSessionRequest, ProcessInfo, ProxyToLayerMessage,
        codec,
    };
    use mirrord_protocol_io::Connection;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::sync::CancellationToken;

    use super::LayerInitializer;
    use crate::{
        ProxyMessage,
        background_tasks::{BackgroundTasks, TaskUpdate},
        error::ProxyRuntimeError,
        main_tasks::MainTaskId,
    };

    /// Shutdown must not wait for a client that connected but never supplied its PID.
    #[tokio::test]
    async fn cancellation_discards_undecoded_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let shutdown = CancellationToken::new();
        let handler = tokio::spawn({
            let shutdown = shutdown.clone();
            async move {
                LayerInitializer::handle_new_stream(stream, address, super::LayerId(0), &shutdown)
                    .await
            }
        });
        shutdown.cancel();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), handler)
                .await
                .expect("undecoded handshake blocked shutdown")
                .unwrap()
                .unwrap()
                .is_none()
        );
        drop(client);
    }

    /// A connection that closes without sending `NewSession` must not stop the initializer from
    /// accepting other layers.
    #[tokio::test]
    async fn survives_connection_closed_before_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (connection, _, _out) = Connection::dummy();
        let mut tasks: BackgroundTasks<MainTaskId, ProxyMessage, ProxyRuntimeError> =
            BackgroundTasks::new(connection.tx_handle());
        let (initializer, _shutdown) = LayerInitializer::new(listener);
        let _initializer = tasks.register(initializer, MainTaskId::LayerInitializer, 32);

        drop(TcpStream::connect(addr).await.unwrap());

        let (mut tx, mut rx) = codec::make_async_framed::<
            LocalMessage<LayerToProxyMessage>,
            LocalMessage<ProxyToLayerMessage>,
        >(TcpStream::connect(addr).await.unwrap());
        tx.send(LocalMessage {
            message_id: 0,
            inner: LayerToProxyMessage::NewSession(NewSessionRequest {
                process_info: ProcessInfo {
                    pid: 1337,
                    parent_pid: 1336,
                    name: "layer".into(),
                    cmdline: vec!["layer".into()],
                    loaded: true,
                },
                parent_layer: None,
            }),
        })
        .await
        .unwrap();

        let (id, update) = tokio::time::timeout(Duration::from_secs(5), tasks.next())
            .await
            .expect("initializer should keep running")
            .unwrap();
        assert_eq!(id, MainTaskId::LayerInitializer);
        match update {
            TaskUpdate::Message(ProxyMessage::NewLayer(new_layer)) => {
                assert_eq!(new_layer.process_info.pid, 1337);
            }
            TaskUpdate::Message(other) => panic!("unexpected message: {other:?}"),
            TaskUpdate::Finished(result) => panic!("initializer finished: {result:?}"),
        }

        match rx.next().await.unwrap().unwrap() {
            LocalMessage {
                message_id: 0,
                inner: ProxyToLayerMessage::NewSession(..),
            } => {}
            other => panic!("unexpected response: {other:?}"),
        }
    }
}
