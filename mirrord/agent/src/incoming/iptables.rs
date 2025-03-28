use std::{
    fmt, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::Not,
};

use mirrord_agent_iptables::{error::IPTablesError, IPTablesWrapper, SafeIpTables};
use nix::sys::socket::{
    self,
    sockopt::{Ip6tOriginalDst, OriginalDst},
    SockaddrIn, SockaddrIn6,
};
use tokio::net::TcpListener;
use tracing::Level;

use super::{PortRedirector, Redirected};

/// A [`PortRedirector`] implementation that uses a [`TcpListener`]
/// and an iptables/ip6tables wrapper to set rules that send traffic to that listener.
pub struct IpTablesRedirector {
    /// For altering iptables/ip6tables rules.
    iptables: Option<SafeIpTables<IPTablesWrapper>>,
    /// Port of [`Self::listener`](Self::listener).
    ///
    /// Kept as a field, so that we don't have to call [`TcpListener::local_addr`]
    /// each time we get a new connection.
    redirect_to: u16,
    /// Listener to which the connections are redirected.
    listener: TcpListener,
    /// Optional comma-seperated list of pod's IPs.
    ///
    /// Used in iptables/ip6tables rules.
    pod_ips: Option<String>,
    /// Whether existing connections should be flushed when adding new redirects.
    flush_connections: bool,
    /// If this redirector is for IPv6 traffic.
    ipv6: bool,
}

impl IpTablesRedirector {
    /// Creates a new redirector.
    ///
    /// # Params
    ///
    /// * `flush_connections` - when a new redirection is created, flush existing connections (based
    ///   on their destination port).
    /// * `pod_ips` - list of pod IPs, will be used in iptables/ip6tables rules.
    /// * `ipv6` - whether to redirect IPv4 or IPv6 traffic.
    #[tracing::instrument(level = Level::DEBUG, ret, err)]
    pub async fn create(
        flush_connections: bool,
        pod_ips: &[IpAddr],
        ipv6: bool,
    ) -> io::Result<Self> {
        let listener_addr = if ipv6 {
            SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0)
        } else {
            SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0)
        };
        let listener = TcpListener::bind(listener_addr).await?;
        let listener_addr = listener.local_addr()?.port();

        let pod_ips = pod_ips
            .iter()
            .filter(|ip| ip.is_ipv6() == ipv6)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");

        Ok(Self {
            iptables: None,
            redirect_to: listener_addr,
            listener,
            pod_ips: pod_ips.is_empty().not().then_some(pod_ips),
            flush_connections,
            ipv6,
        })
    }
}

impl PortRedirector for IpTablesRedirector {
    type Error = IPTablesError;

    #[tracing::instrument(level = Level::DEBUG, err, ret)]
    async fn add_redirection(&mut self, from_port: u16) -> Result<(), Self::Error> {
        let iptables = match self.iptables.as_ref() {
            Some(iptables) => iptables,
            None => {
                let iptables = if self.ipv6 {
                    mirrord_agent_iptables::new_ip6tables()
                } else {
                    mirrord_agent_iptables::new_iptables()
                };

                let safe = SafeIpTables::create(
                    iptables.into(),
                    self.flush_connections,
                    self.pod_ips.as_deref(),
                    self.ipv6,
                )
                .await?;

                self.iptables.insert(safe)
            }
        };

        iptables.add_redirect(from_port, self.redirect_to).await
    }

    #[tracing::instrument(level = Level::DEBUG, err, ret)]
    async fn remove_redirection(&mut self, from_port: u16) -> Result<(), Self::Error> {
        if let Some(iptables) = self.iptables.as_ref() {
            iptables
                .remove_redirect(from_port, self.redirect_to)
                .await?;
        }

        Ok(())
    }

    #[tracing::instrument(level = Level::DEBUG, err, ret)]
    async fn cleanup(&mut self) -> Result<(), Self::Error> {
        if let Some(iptables) = self.iptables.take() {
            iptables.cleanup().await?;
        }

        Ok(())
    }

    async fn next_connection(&mut self) -> Result<Redirected, Self::Error> {
        loop {
            let (stream, source) = self.listener.accept().await?;

            let destination = if source.is_ipv6() {
                socket::getsockopt(&stream, Ip6tOriginalDst)
                    .map(SockaddrIn6::from)
                    .map(|addr| SocketAddr::new(addr.ip().into(), addr.port()))
            } else {
                socket::getsockopt(&stream, OriginalDst)
                    .map(SockaddrIn::from)
                    .map(|addr| SocketAddr::new(addr.ip().into(), addr.port()))
            };

            match destination {
                Ok(destination) => {
                    break Ok(Redirected {
                        stream,
                        source,
                        destination,
                    })
                }
                Err(error) => {
                    // Resolving the original destination can fail,
                    // e.g if someone made connection directly to our socket.
                    // However, as it is very unlikely, we log this as an error.
                    tracing::error!(
                        %error,
                        connection_source = %source,
                        "Failed to obtain the original destination of a redirected TCP connection. \
                        Dropping the connection.",
                    );
                }
            }
        }
    }
}

impl fmt::Debug for IpTablesRedirector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IpTablesRedirector")
            .field("redirect_to", &self.redirect_to)
            .field("pod_ips", &self.pod_ips)
            .field("flush_connections", &self.flush_connections)
            .field("ipv6", &self.ipv6)
            .finish()
    }
}
