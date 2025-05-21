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
    incoming::{
        tls::IncomingTlsHandlerStore, BoxBody, MirrorHandle, PortRedirector, Redirected,
        RedirectorTask, StealHandle,
    },
};

/// Implementation of [`PortRedirector`] that can be used in unit tests.
pub struct DummyRedirector {
    state: watch::Sender<DummyRedirectorState>,
    conn_rx: mpsc::Receiver<Redirected>,
}

impl DummyRedirector {
    /// Creates a new dummy redirector.
    ///
    /// Returns:
    /// 1. the redirector,
    /// 2. a [`watch::Receiver`] that can be used to inspect the redirector's state,
    /// 3. a [`DummyConnections`] struct that can be used to send mocked [`Redirected`] connections
    ///    through the redirector.
    fn new() -> (
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
        let conn = self.conn_rx.recv().await;

        if let Some(conn) = conn {
            Ok(conn)
        } else {
            std::future::pending().await
        }
    }
}

/// State of [`DummyRedirector`].
#[derive(Default)]
pub struct DummyRedirectorState {
    pub dirty: bool,
    pub redirections: HashSet<u16>,
}

impl DummyRedirectorState {
    /// Checks whether this state contains exactly the given redirections.
    ///
    /// If `redirections` is empty, checks if the state is clean.
    pub fn has_redirections<I>(&self, redirections: I) -> bool
    where
        I: IntoIterator<Item = u16>,
    {
        let expected = redirections.into_iter().collect::<HashSet<_>>();

        self.redirections == expected && expected.is_empty() != self.dirty
    }
}

/// Can be used in unit tests to feed the associated [`RedirectorTask`] with connections.
pub struct DummyConnections {
    tx: mpsc::Sender<Redirected>,
    ipv4_listener: Option<TcpListener>,
    ipv6_listener: Option<TcpListener>,
    redirector_state: watch::Receiver<DummyRedirectorState>,
}

impl DummyConnections {
    /// Creates a new TCP connection and sends it to the redirector.
    ///
    /// `destination` is the mocked original destination of the connection.
    ///
    /// Returns the client stream.
    pub async fn new_tcp(&mut self, destination: SocketAddr) -> TcpStream {
        let listener = if destination.is_ipv4() {
            match self.ipv4_listener.as_ref() {
                Some(listener) => listener,
                None => {
                    let listener =
                        TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
                            .await
                            .unwrap();
                    self.ipv4_listener.insert(listener)
                }
            }
        } else {
            match self.ipv6_listener.as_ref() {
                Some(listener) => listener,
                None => {
                    let listener =
                        TcpListener::bind(SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 0))
                            .await
                            .unwrap();
                    self.ipv6_listener.insert(listener)
                }
            }
        };

        let addr = listener.local_addr().unwrap();
        let ((server_stream, source), client_stream) =
            tokio::try_join!(listener.accept(), TcpStream::connect(addr)).unwrap();

        let redirected = Redirected {
            stream: server_stream,
            source,
            destination,
        };

        self.tx.send(redirected).await.unwrap();

        client_stream
    }

    /// Creates a new HTTP connection and sends it to the redirector.
    ///
    /// `destination` is the mocked original destination of the connection.
    ///
    /// Returns the request sender.
    pub async fn new_http(
        &mut self,
        destination: SocketAddr,
        version: HttpVersion,
    ) -> HttpSender<BoxBody> {
        let stream = self.new_tcp(destination).await;

        match version {
            HttpVersion::V1 => {
                let (sender, conn) =
                    hyper::client::conn::http1::handshake::<_, BoxBody>(TokioIo::new(stream))
                        .await
                        .unwrap();
                tokio::spawn(conn.with_upgrades());
                HttpSender::V1(sender)
            }
            HttpVersion::V2 => {
                let (sender, conn) = hyper::client::conn::http2::handshake::<_, _, BoxBody>(
                    TokioExecutor::new(),
                    TokioIo::new(stream),
                )
                .await
                .unwrap();
                tokio::spawn(conn);
                HttpSender::V2(sender)
            }
        }
    }

    /// Returns a view into the mocked [`PortRedirector`] state.
    pub fn redirector_state(&mut self) -> &mut watch::Receiver<DummyRedirectorState> {
        &mut self.redirector_state
    }
}

/// Spawns a [`RedirectorTask`] that uses a mocked [`PortRedirector`] implementation.
///
/// Returned [`DummyConnections`] can be used to inspect [`PortRedirector`] state
/// and send mocked connections to the [`RedirectorTask`].
pub async fn start_redirector_task(
    tls_handlers: IncomingTlsHandlerStore,
) -> (DummyConnections, StealHandle, MirrorHandle) {
    let (redirector, redirector_state, conn_tx) = DummyRedirector::new();
    let (task, steal_handle, mirror_handle) = RedirectorTask::new(redirector, tls_handlers);
    tokio::spawn(task.run());

    (
        DummyConnections {
            tx: conn_tx,
            ipv4_listener: None,
            ipv6_listener: None,
            redirector_state,
        },
        steal_handle,
        mirror_handle,
    )
}
