//! A fake intproxy that talks to the layer directly, with the `layer <-> proxy` protocol.
//!
//! [`TestIntProxy`](crate::intproxy::TestIntProxy) runs the real intproxy, which reads the
//! connections of the layer all the time. Some tests need the writes of the layer to block, for
//! example to keep a `close` hook in the middle of its request to the intproxy, and to check what
//! other threads of the application can do at that time. These tests use [`FakeIntProxy`]: while a
//! test does not read a [`FakeLayerConnection`], the writes of the layer to it block when the
//! socket buffer is full.

use std::{net::SocketAddr, time::Duration};

use futures::{SinkExt, StreamExt};
use mirrord_intproxy_protocol::{
    LayerId, LayerToProxyMessage, LocalMessage, MessageId, NewSessionRequest, ProxyToLayerMessage,
    codec::{AsyncDecoder, AsyncEncoder, make_async_framed},
};
use tokio::net::{
    TcpListener, TcpStream,
    tcp::{OwnedReadHalf, OwnedWriteHalf},
};

/// Accepts the connections of the layer. Each process with the layer (also a forked child) makes
/// its own connection.
pub struct FakeIntProxy {
    listener: TcpListener,
    next_layer_id: u64,
}

impl FakeIntProxy {
    pub async fn new() -> Self {
        Self {
            listener: TcpListener::bind("127.0.0.1:0").await.unwrap(),
            next_layer_id: 0,
        }
    }

    /// The address to give to the layer as its intproxy address.
    pub fn address(&self) -> SocketAddr {
        self.listener.local_addr().unwrap()
    }

    /// Accepts the next layer connection, and answers its [`NewSessionRequest`]. Gives [`None`]
    /// when no layer connects within `timeout`.
    pub async fn try_accept(&mut self, timeout: Duration) -> Option<FakeLayerConnection> {
        let (stream, _) = tokio::time::timeout(timeout, self.listener.accept())
            .await
            .ok()?
            .unwrap();
        Some(self.start_session(stream).await)
    }

    async fn start_session(&mut self, stream: TcpStream) -> FakeLayerConnection {
        let (mut tx, mut rx) = make_async_framed::<
            LocalMessage<ProxyToLayerMessage>,
            LocalMessage<LayerToProxyMessage>,
        >(stream);

        let message = rx.next().await.unwrap().unwrap();
        let LayerToProxyMessage::NewSession(session) = message.inner else {
            panic!("expected a `NewSessionRequest` from the layer, got {message:?}");
        };

        let id = LayerId(self.next_layer_id);
        self.next_layer_id += 1;
        tx.send(LocalMessage {
            message_id: message.message_id,
            inner: ProxyToLayerMessage::NewSession(id),
        })
        .await
        .unwrap();

        FakeLayerConnection {
            id,
            session,
            tx,
            rx,
        }
    }
}

/// The connection of one layer to the [`FakeIntProxy`].
pub struct FakeLayerConnection {
    pub id: LayerId,
    /// The first request of the layer on this connection.
    pub session: NewSessionRequest,
    tx: AsyncEncoder<LocalMessage<ProxyToLayerMessage>, OwnedWriteHalf>,
    rx: AsyncDecoder<LocalMessage<LayerToProxyMessage>, OwnedReadHalf>,
}

impl FakeLayerConnection {
    /// Reads the next message of the layer. Gives [`None`] when the layer closed the connection.
    pub async fn recv(&mut self) -> Option<LocalMessage<LayerToProxyMessage>> {
        self.rx.next().await.map(Result::unwrap)
    }

    /// Answers the request of the layer with the given `message_id`.
    pub async fn send(&mut self, message_id: MessageId, inner: ProxyToLayerMessage) {
        self.tx
            .send(LocalMessage { message_id, inner })
            .await
            .unwrap();
    }
}
