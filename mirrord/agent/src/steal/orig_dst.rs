use std::{io, net::SocketAddr};

use nix::sys::socket::{
    sockopt::{Ip6tOriginalDst, OriginalDst},
    SockaddrIn, SockaddrIn6,
};
use tokio::net::TcpStream;

/// Returns the original destination of a [`TcpStream`] redirected with iptables/ip6tables
/// `REDIRECT`.
///
/// If [`TcpStream::local_addr`] is an IPv6 address,
/// this function assumes that the connection was redirected using ip6tables.
///
/// If [`TcpStream::local_addr`] is an IPv4 address,
/// this function assumes that the connection was redirected using iptables.
pub(super) fn orig_dst_addr(stream: &TcpStream) -> io::Result<SocketAddr> {
    if stream.local_addr()?.is_ipv6() {
        let raw = nix::sys::socket::getsockopt(stream, Ip6tOriginalDst)?;
        let addr = SockaddrIn6::from(raw);
        Ok(SocketAddr::new(addr.ip().into(), addr.port()))
    } else {
        let raw = nix::sys::socket::getsockopt(stream, OriginalDst)?;
        let addr = SockaddrIn::from(raw);
        Ok(SocketAddr::new(addr.ip().into(), addr.port()))
    }
}
