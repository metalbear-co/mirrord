//! This module contains components that implement redirecting incoming traffic.

mod composed;
mod connection;
mod error;
mod iptables;
mod mirror_handle;
mod steal_handle;
mod task;
pub mod tls;

use std::{
    fmt,
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
};

use composed::ComposedRedirector;
pub use connection::{
    IncomingStream, IncomingStreamItem,
    http::{BypassedHttp, ResponseBodyProvider, ResponseProvider, StolenHttp},
    tcp::StolenTcp,
};
pub use error::{ConnError, RedirectorTaskError};
use iptables::IpTablesRedirector;
pub use mirror_handle::{MirrorHandle, MirroredTraffic};
pub use steal_handle::{StealHandle, StolenTraffic};
pub use task::{RedirectorTask, RedirectorTaskConfig};
use tokio::net::TcpStream;

/// A component that implements redirecting incoming TCP connections.
pub trait PortRedirector {
    type Error: Sized;

    fn initialize(&mut self) -> impl Future<Output = Result<(), Self::Error>> {
        std::future::ready(Ok(()))
    }

    /// Start redirecting connections from the given port.
    ///
    /// # Note
    ///
    /// If a redirection from the given port already exists, implementations are free to do nothing
    /// or return an [`Err`].
    fn add_redirection(&mut self, from_port: u16) -> impl Future<Output = Result<(), Self::Error>>;

    /// Stop redirecting connections from the given port.
    ///
    /// # Note
    ///
    /// If the redirection does no exist, implementations are free to do nothing or return an
    /// [`Err`].
    fn remove_redirection(
        &mut self,
        from_port: u16,
    ) -> impl Future<Output = Result<(), Self::Error>>;

    /// Clean any external state.
    fn cleanup(&mut self) -> impl Future<Output = Result<(), Self::Error>>;

    /// Accept an incoming redirected connection.
    ///
    /// Implementors are allowed to return a connection to a port that is no longer redirected.
    fn next_connection(&mut self) -> impl Future<Output = Result<Redirected, Self::Error>>;
}

/// A redirected TCP connection.
///
/// Returned from [`PortRedirector::next_connection`].
pub struct Redirected {
    /// IO stream.
    stream: TcpStream,
    /// Source of the connection.
    source: SocketAddr,
    /// Destination of the connection.
    ///
    /// Note that this address might be different than the local address of [`Self::stream`].
    destination: SocketAddr,
}

impl fmt::Debug for Redirected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Redirected")
            .field("source", &self.source)
            .field("destination", &self.destination)
            .finish()
    }
}

/// Creates a [`ComposedRedirector`] based on [`IpTablesRedirector`]s.
///
/// Fails when no inner redirector can be created.
///
/// # Params
///
/// * `flush_connections` - passed to inner redirectors.
/// * `pod_ips` - passed to inner redirectors.
/// * `support_ipv6` - if set, this function will attempt to create both an IPv4 and an IPv6
///   redirector. Otherwise, it will only attempt to create an IPv4 redirector.
pub async fn create_iptables_redirector(
    flush_connections: bool,
    pod_ips: &[IpAddr],
    support_ipv6: bool,
    with_mesh_exclusion: Option<u16>,
) -> io::Result<ComposedRedirector<IpTablesRedirector>> {
    let ipv4 = IpTablesRedirector::create(flush_connections, pod_ips, false, with_mesh_exclusion)
        .await
        .inspect_err(|error| {
            tracing::error!(
                %error,
                "Failed to create an IPv4 traffic redirector",
            )
        });

    let ipv6 = if support_ipv6 {
        IpTablesRedirector::create(flush_connections, pod_ips, true, with_mesh_exclusion)
            .await
            .inspect_err(|error| {
                tracing::error!(
                    %error,
                    "Failed to create an IPv6 traffic redirector",
                )
            })
            .into()
    } else {
        None
    };

    let redirectors = match (ipv4, ipv6) {
        (Ok(ipv4), ipv6) => {
            let mut redirectors = vec![ipv4];
            redirectors.extend(ipv6.transpose().ok().flatten());
            redirectors
        }
        (Err(error), None | Some(Err(..))) => return Err(error),
        (Err(..), Some(Ok(ipv6))) => vec![ipv6],
    };

    Ok(ComposedRedirector::new(redirectors))
}

#[cfg(test)]
pub mod test {
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

    use super::{PortRedirector, Redirected};

    /// Implementation of [`PortRedirector`] that can be used in unit tests.
    /// Receives connections sent from [`DummyConnectionTx`].
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
        /// 3. an [`mpsc::Sender`] that can be used to send mocked [`Redirected`] connections
        ///    through the redirector.
        pub fn new() -> (
            Self,
            watch::Receiver<DummyRedirectorState>,
            DummyConnectionTx,
        ) {
            let (conn_tx, conn_rx) = mpsc::channel(8);
            let (state_tx, state_rx) = watch::channel(DummyRedirectorState::default());

            (
                Self {
                    state: state_tx,
                    conn_rx,
                },
                state_rx,
                DummyConnectionTx {
                    tx: conn_tx,
                    v4_listener: None,
                    v6_listener: None,
                },
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

    /// Used for simulating incoming connections from the outside world.
    pub struct DummyConnectionTx {
        tx: mpsc::Sender<Redirected>,
        v4_listener: Option<TcpListener>,
        v6_listener: Option<TcpListener>,
    }

    impl DummyConnectionTx {
        pub async fn make_connection(&mut self, original_destination: SocketAddr) -> TcpStream {
            let (listener, addr) = if original_destination.is_ipv4() {
                (
                    &mut self.v4_listener,
                    SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
                )
            } else {
                (
                    &mut self.v6_listener,
                    SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 0),
                )
            };

            let listener = match listener {
                Some(listener) => listener,
                None => {
                    let new_listener = TcpListener::bind(addr).await.unwrap();
                    listener.insert(new_listener)
                }
            };

            let ((server_stream, peer_addr), client_stream) = tokio::try_join!(
                listener.accept(),
                TcpStream::connect(listener.local_addr().unwrap()),
            )
            .unwrap();

            let redirected = Redirected {
                stream: server_stream,
                source: peer_addr,
                destination: original_destination,
            };
            self.tx.send(redirected).await.unwrap();

            client_stream
        }
    }
}
