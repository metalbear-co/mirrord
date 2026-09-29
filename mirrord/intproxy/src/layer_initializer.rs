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
                    // A failed handshake only affects the connecting process.
                    match self.handle_new_stream(stream, layer_address).await {
                        Ok(new_layer) => message_bus.send(new_layer).await,
                        Err(error) => tracing::warn!(
                            %error,
                            %layer_address,
                            "Failed to initialize a layer connection, dropping it"
                        ),
                    }
                },
            }
        }
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

    use super::LayerInitializer;
    use crate::{
        ProxyMessage,
        background_tasks::{BackgroundTasks, TaskUpdate},
        error::ProxyRuntimeError,
        main_tasks::MainTaskId,
    };

    /// A connection that closes without sending `NewSession` must not stop the initializer from
    /// accepting other layers.
    #[tokio::test]
    async fn survives_connection_closed_before_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (connection, _, _out) = Connection::dummy();
        let mut tasks: BackgroundTasks<MainTaskId, ProxyMessage, ProxyRuntimeError> =
            BackgroundTasks::new(connection.tx_handle());
        let _initializer = tasks.register(
            LayerInitializer::new(listener),
            MainTaskId::LayerInitializer,
            32,
        );

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
