//! This module contains components that implement redirecting incoming traffic.

mod composed;
mod connection;
mod error;
mod iptables;
mod mirror_handle;
mod steal_handle;
mod task;

use std::{
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
};

use composed::ComposedRedirector;
pub use connection::{
    http::{BoxBody, MirroredHttp, ResponseProvider, StolenHttp},
    tcp::{MirroredTcp, StolenTcp},
    IncomingStream, IncomingStreamItem,
};
pub use error::{ConnError, RedirectorTaskError};
use iptables::IpTablesRedirector;
pub use mirror_handle::{MirrorHandle, MirroredTraffic};
pub use steal_handle::{StealHandle, StolenTraffic};
pub use task::RedirectorTask;
use tokio::net::TcpStream;

/// A component that implements redirecting incoming TCP connections.
pub trait PortRedirector {
    type Error: Sized;

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
#[derive(Debug)]
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
) -> io::Result<ComposedRedirector<IpTablesRedirector>> {
    let ipv4 = IpTablesRedirector::create(flush_connections, pod_ips, false)
        .await
        .inspect_err(|error| {
            tracing::error!(
                %error,
                "Failed to create an IPv4 traffic redirector",
            )
        });

    let ipv6 = if support_ipv6 {
        IpTablesRedirector::create(flush_connections, pod_ips, true)
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
}
