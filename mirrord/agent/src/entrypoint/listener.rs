//! Accepting client connections to the agent.
//!
//! The agent always listens for TCP connections on its client port. When it was given an operator
//! certificate to pin, it additionally listens for QUIC connections on the same port number, over
//! UDP. Which transport a client uses is its own choice: an operator that predates the QUIC
//! transport, or one that cannot reach the agent over UDP, simply connects over TCP and never
//! sends a packet to the UDP socket. There is nothing to negotiate before the connection exists,
//! which is what makes the fallback work without a version handshake.
//!
//! The QUIC socket is bound in the agent's own network namespace, like the TCP one. Only the
//! background task runtime enters the target's namespace.

use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
    os::fd::AsFd,
    sync::Arc,
};

use socket2::SockRef;
use tokio::{
    net::{TcpListener, TcpSocket, TcpStream},
    select,
};
use tracing::Level;

use crate::error::{AgentError, AgentResult};

/// A client connection that has arrived but has not been set up yet.
///
/// Handshakes are deliberately left to the per-client task: a QUIC handshake involves several
/// round trips, and doing it on the accept path would let one slow client hold up every other
/// client waiting to connect.
pub(super) enum IncomingClient {
    /// A TCP connection, which may still need a TLS handshake.
    Tcp(TcpStream),
    /// A QUIC connection that has not completed its handshake.
    Quic(Box<quinn::Incoming>),
}

/// Listens for client connections on both transports the agent supports.
pub(super) struct ClientListener {
    tcp: TcpListener,
    /// [`None`] when the agent has no operator certificate to pin, since without one there is no
    /// way to tell the operator apart from any other pod that can reach this port.
    quic: Option<quinn::Endpoint>,
}

impl ClientListener {
    /// Binds both listeners to `port`.
    ///
    /// `operator_cert_pem` is the certificate the agent pins, from
    /// [`OPERATOR_CERT`](mirrord_agent_env::envs::OPERATOR_CERT). Failing to set up QUIC is not
    /// fatal: the agent logs it and serves TCP clients only.
    #[tracing::instrument(level = Level::DEBUG, skip(operator_cert_pem), err)]
    pub(super) fn bind(port: u16, operator_cert_pem: Option<&str>) -> AgentResult<Self> {
        let tcp = bind_tcp(port)?;

        let quic = operator_cert_pem.and_then(|cert| match bind_quic(port, cert) {
            Ok(endpoint) => Some(endpoint),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "Failed to set up the QUIC client listener, serving TCP clients only.",
                );
                None
            }
        });

        Ok(Self { tcp, quic })
    }

    /// Address of the TCP listener.
    ///
    /// The QUIC listener uses the same port, so this is also where QUIC clients connect.
    pub(super) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp.local_addr()
    }

    /// Waits for the next client on either transport.
    ///
    /// # Cancel safety
    ///
    /// This function is cancel safe. If it is dropped before completing, no connection is lost:
    /// both transports keep the pending connection queued.
    pub(super) async fn accept(&mut self) -> io::Result<IncomingClient> {
        loop {
            let Some(endpoint) = self.quic.as_ref() else {
                let (stream, _) = self.tcp.accept().await?;
                return Ok(IncomingClient::Tcp(stream));
            };

            let quic_closed = select! {
                accepted = self.tcp.accept() => {
                    let (stream, _) = accepted?;
                    return Ok(IncomingClient::Tcp(stream));
                },

                incoming = endpoint.accept() => match incoming {
                    Some(incoming) => return Ok(IncomingClient::Quic(Box::new(incoming))),
                    // The endpoint is closed and will never yield another connection. Forget it,
                    // so that the next iteration does not spin on an immediately ready future.
                    None => true,
                },
            };

            if quic_closed {
                tracing::debug!("QUIC client listener closed, serving TCP clients only.");
                self.quic = None;
            }
        }
    }
}

/// Prefers a dual-stack IPv6 socket so both IPv4 and IPv6 clients can connect. If anything in that
/// setup fails (e.g. IPv6 is disabled in the cluster), falls back to a plain IPv4 socket.
///
/// `prepare` sets any socket options that must be applied before binding, and binds the socket to
/// the given address.
fn bind_dual_stack<T: AsFd>(
    port: u16,
    new_v6: impl Fn() -> io::Result<T>,
    new_v4: impl Fn() -> io::Result<T>,
    prepare: impl Fn(&T, SocketAddr) -> io::Result<()>,
) -> io::Result<T> {
    let dual_stack = new_v6().and_then(|socket| {
        // Accept IPv4 clients on this socket as well, not only IPv6 ones.
        SockRef::from(&socket).set_only_v6(false)?;
        prepare(&socket, SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), port))?;
        Ok(socket)
    });

    match dual_stack {
        Ok(socket) => Ok(socket),
        Err(error) => {
            tracing::warn!(%error, "Failed to set up an IPv6 client listener, falling back to IPv4.");
            let socket = new_v4()?;
            prepare(&socket, SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port))?;
            Ok(socket)
        }
    }
}

fn bind_tcp(port: u16) -> AgentResult<TcpListener> {
    let socket = bind_dual_stack(
        port,
        TcpSocket::new_v6,
        TcpSocket::new_v4,
        |socket, address| {
            // SO_REUSEADDR is required to handle rapid agent restarts.
            socket.set_reuseaddr(true)?;
            socket.bind(address)
        },
    )?;

    socket.listen(1024).map_err(AgentError::from)
}

fn bind_quic(port: u16, operator_cert_pem: &str) -> AgentResult<quinn::Endpoint> {
    let socket = bind_dual_stack(
        port,
        || socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::DGRAM, None),
        || socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None),
        |socket, address| {
            // SO_REUSEADDR is required to handle rapid agent restarts.
            socket.set_reuse_address(true)?;
            socket.bind(&address.into())
        },
    )?;
    socket.set_nonblocking(true)?;

    let config = mirrord_quic::server_config(operator_cert_pem)
        .map_err(|error| io::Error::other(error.to_string()))?;

    quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(config),
        UdpSocket::from(socket),
        Arc::new(quinn::TokioRuntime),
    )
    .map_err(AgentError::from)
}
