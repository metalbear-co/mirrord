//! This module contains components that implement redirecting incoming traffic.

mod composed;
mod connection;
mod error;
mod iptables;
mod mirror_handle;
mod steal_handle;
mod task;
#[cfg(test)]
pub mod test;
mod tls;

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
pub use tls::IncomingTlsHandlerStore;
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
