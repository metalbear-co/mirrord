use std::{
    fmt, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use tokio::net::{TcpSocket, TcpStream};
use tracing::Level;

/// A TCP socket that is already bound.
///
/// Provides a nicer [`fmt::Debug`] implementation than [`TcpSocket`].
pub struct BoundTcpSocket(TcpSocket);

impl BoundTcpSocket {
    /// Opens a new TCP socket and binds it to the given IP address and a random port.
    /// If the given IP address is not specified, binds the socket to localhost instead.
    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub fn bind_specified_or_localhost(ip: IpAddr) -> io::Result<Self> {
        let (socket, ip) = match ip {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED) => (TcpSocket::new_v4()?, Ipv4Addr::LOCALHOST.into()),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED) => (TcpSocket::new_v6()?, Ipv6Addr::LOCALHOST.into()),
            addr @ IpAddr::V4(..) => (TcpSocket::new_v4()?, addr),
            addr @ IpAddr::V6(..) => (TcpSocket::new_v6()?, addr),
        };

        socket.bind(SocketAddr::new(ip, 0))?;

        Ok(Self(socket))
    }

    /// Returns the address to which this socket is bound.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }

    /// Makes a connection to the given peer.
    pub async fn connect(self, peer: SocketAddr) -> io::Result<TcpStream> {
        self.0.connect(peer).await
    }
}

impl fmt::Debug for BoundTcpSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.local_addr().fmt(f)
    }
}
