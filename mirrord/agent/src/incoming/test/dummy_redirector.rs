use std::{
    collections::HashSet,
    error::Error,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::Not,
};

use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
};

use crate::{
    http::{HttpSender, HttpVersion},
    incoming::{BoxBody, PortRedirector, Redirected},
};

/// Implementation of [`PortRedirector`] that can be used in unit tests.
pub struct DummyRedirector {
    state: watch::Sender<DummyRedirectorState>,
    conn_rx: mpsc::Receiver<Redirected>,
}

/// State of [`DummyRedirector`].
#[derive(Default)]
pub struct DummyRedirectorState {
    pub dirty: bool,
    pub redirections: HashSet<u16>,
}

impl DummyRedirectorState {
    pub fn has_redirections<I>(&self, redirections: I) -> bool
    where
        I: IntoIterator<Item = u16>,
    {
        let expected = redirections.into_iter().collect::<HashSet<_>>();

        self.redirections == expected && expected.is_empty() != self.dirty
    }
}

impl DummyRedirector {
    /// Creates a new dummy redirector.
    ///
    /// Returns:
    /// 1. the redirector,
    /// 2. a [`watch::Receiver`] that can be used to inspect the redirector's state,
    /// 3. a [`DummyConnections`] struct that can be used to send mocked [`Redirected`] connections
    ///    through the redirector.
    pub fn new() -> (
        Self,
        watch::Receiver<DummyRedirectorState>,
        DummyConnections,
    ) {
        let (conn_tx, conn_rx) = mpsc::channel(8);
        let (state_tx, state_rx) = watch::channel(DummyRedirectorState::default());

        (
            Self {
                state: state_tx,
                conn_rx,
            },
            state_rx,
            DummyConnections { tx: conn_tx },
        )
    }
}

impl PortRedirector for DummyRedirector {
    type Error = Box<dyn Error + Send + Sync + 'static>;

    async fn add_redirection(&mut self, from_port: u16) -> Result<(), Self::Error> {
        let changed = self.state.send_if_modified(|state| {
            if state.redirections.insert(from_port) {
                state.dirty = true;
                true
            } else {
                false
            }
        });

        if changed {
            Ok(())
        } else {
            Err(format!("{from_port} was already redirected").into())
        }
    }

    async fn remove_redirection(&mut self, from_port: u16) -> Result<(), Self::Error> {
        let changed = self
            .state
            .send_if_modified(|state| state.redirections.remove(&from_port));

        if changed {
            Ok(())
        } else {
            Err(format!("{from_port} was not redirected").into())
        }
    }

    async fn cleanup(&mut self) -> Result<(), Self::Error> {
        self.state.send_if_modified(|state| {
            let changed = state.dirty || state.redirections.is_empty().not();
            state.dirty = false;
            state.redirections.clear();
            changed
        });

        Ok(())
    }

    async fn next_connection(&mut self) -> Result<Redirected, Self::Error> {
        let conn = self.conn_rx.recv().await;

        if let Some(conn) = conn {
            return Ok(conn);
        } else {
            return std::future::pending().await;
        }
    }
}

impl Redirected {
    async fn dummy(destination: SocketAddr) -> (Self, TcpStream) {
        let listen_addr = if destination.is_ipv4() {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)
        } else {
            SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 0)
        };
        let listener = TcpListener::bind(listen_addr).await.unwrap();
        let listen_addr = listener.local_addr().unwrap();

        let client_stream = tokio::spawn(TcpStream::connect(listen_addr));

        let (server_stream, source) = listener.accept().await.unwrap();

        (
            Self {
                stream: server_stream,
                source,
                destination,
            },
            client_stream.await.unwrap().unwrap(),
        )
    }
}

pub struct DummyConnections {
    tx: mpsc::Sender<Redirected>,
}

impl DummyConnections {
    pub async fn new_tcp(&self, destination: SocketAddr) -> TcpStream {
        let (redirected, client_side) = Redirected::dummy(destination).await;
        self.tx.send(redirected).await.unwrap();
        client_side
    }

    pub async fn new_http(
        &self,
        destination: SocketAddr,
        version: HttpVersion,
    ) -> HttpSender<BoxBody> {
        let (redirected, client_side) = Redirected::dummy(destination).await;
        self.tx.send(redirected).await.unwrap();

        match version {
            HttpVersion::V1 => {
                let (sender, conn) =
                    hyper::client::conn::http1::handshake::<_, BoxBody>(TokioIo::new(client_side))
                        .await
                        .unwrap();
                tokio::spawn(conn.with_upgrades());
                HttpSender::V1(sender)
            }
            HttpVersion::V2 => {
                let (sender, conn) = hyper::client::conn::http2::handshake::<_, _, BoxBody>(
                    TokioExecutor::new(),
                    TokioIo::new(client_side),
                )
                .await
                .unwrap();
                tokio::spawn(conn);
                HttpSender::V2(sender)
            }
        }
    }
}
