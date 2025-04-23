use std::{
    collections::HashSet,
    error::Error,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::Not,
};

use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
};

use crate::incoming::{PortRedirector, Redirected};

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
    /// 3. an [`mpsc::Sender`] that can be used to send mocked [`Redirected`] connections through
    ///    the redirector.
    pub fn new() -> (
        Self,
        watch::Receiver<DummyRedirectorState>,
        mpsc::Sender<Redirected>,
    ) {
        let (conn_tx, conn_rx) = mpsc::channel(8);
        let (state_tx, state_rx) = watch::channel(DummyRedirectorState::default());

        (
            Self {
                state: state_tx,
                conn_rx,
            },
            state_rx,
            conn_tx,
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
        self.conn_rx
            .recv()
            .await
            .ok_or_else(|| "channel closed".into())
    }
}

impl Redirected {
    pub async fn dummy(destination: SocketAddr) -> (Self, TcpStream) {
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
