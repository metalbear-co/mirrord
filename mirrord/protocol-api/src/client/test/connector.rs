use std::{
    io,
    ops::Not,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{Sink, SinkExt, Stream, StreamExt, channel::mpsc};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::sync::oneshot;

use crate::client::ProtocolConnector;

/// Test implementation of [`ProtocolConnector`].
///
/// Uses [`futures::channel::mpsc`] channels internally,
/// as they natively implement [`Stream`] and [`Sync`].
#[derive(Debug)]
pub struct TestConnector {
    connect_tx: mpsc::UnboundedSender<oneshot::Sender<TestConn>>,
}

impl TestConnector {
    pub fn new_pair() -> (Self, TestAcceptor) {
        let (connect_tx, connect_rx) = mpsc::unbounded();
        (Self { connect_tx }, TestAcceptor { connect_rx })
    }
}

impl ProtocolConnector for TestConnector {
    type Error = io::Error;
    type Conn = TestConn;

    async fn connect(&mut self) -> Result<Self::Conn, Self::Error> {
        let (tx, rx) = oneshot::channel();
        self.connect_tx.send(tx).await.ok();
        rx.await
            .map_err(|_| io::Error::other("reconnect oneshot dropped"))
    }

    fn can_reconnect(&self) -> bool {
        self.connect_tx.is_closed().not()
    }
}

/// Implementation of [`ProtocolConnector::Conn`] for [`TestConnector`].
#[derive(Debug)]
pub struct TestConn {
    sink: mpsc::UnboundedSender<ClientMessage>,
    stream: mpsc::UnboundedReceiver<io::Result<DaemonMessage>>,
}

impl TestConn {
    pub fn new_pair() -> (Self, TestServer) {
        let (client_sink, client_stream) = mpsc::unbounded();
        let (daemon_sink, daemon_stream) = mpsc::unbounded();
        (
            Self {
                sink: client_sink,
                stream: daemon_stream,
            },
            TestServer {
                sink: daemon_sink,
                stream: client_stream,
            },
        )
    }
}

impl Stream for TestConn {
    type Item = io::Result<DaemonMessage>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().stream.poll_next_unpin(cx)
    }
}

impl Sink<ClientMessage> for TestConn {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut()
            .sink
            .poll_ready_unpin(cx)
            .map_err(|_| io::Error::other("channel closed"))
    }

    fn start_send(self: Pin<&mut Self>, item: ClientMessage) -> Result<(), Self::Error> {
        self.get_mut()
            .sink
            .start_send_unpin(item)
            .map_err(|_| io::Error::other("channel closed"))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut()
            .sink
            .poll_flush_unpin(cx)
            .map_err(|_| io::Error::other("channel closed"))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.get_mut()
            .sink
            .poll_close_unpin(cx)
            .map_err(|_| io::Error::other("channel closed"))
    }
}

/// Test implementation of the other side of [`TestConnector`].
///
/// Dropping it will signal the [`TestConnector`] that no more reconnect attempts should be made.
#[derive(Debug)]
pub struct TestAcceptor {
    connect_rx: mpsc::UnboundedReceiver<oneshot::Sender<TestConn>>,
}

impl TestAcceptor {
    /// Accepts the next [`ProtocolConnector::connect`] attempt from the sibling [`TestConnector`].
    pub async fn accept(&mut self, server_version: semver::Version) -> TestServer {
        let conn_tx = self.connect_rx.next().await.unwrap();
        let (conn, mut server) = TestConn::new_pair();
        conn_tx.send(conn).ok();

        let version = match server.stream.next().await.unwrap() {
            ClientMessage::SwitchProtocolVersion(client_version) => {
                std::cmp::min(client_version, server_version)
            }
            other => panic!("unexpected message: {other:?}"),
        };

        server
            .sink
            .send(Ok(DaemonMessage::SwitchProtocolVersionResponse(version)))
            .await
            .unwrap();

        server
    }
}

/// Test implementation of [`mirrord_protocol`] server, the other side of [`TestConn`].
#[derive(Debug)]
pub struct TestServer {
    pub sink: mpsc::UnboundedSender<io::Result<DaemonMessage>>,
    pub stream: mpsc::UnboundedReceiver<ClientMessage>,
}
