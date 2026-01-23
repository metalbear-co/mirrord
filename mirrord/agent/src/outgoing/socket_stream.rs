use std::{
    ffi::OsStr,
    io::{self, Error},
    net::IpAddr,
    os::{
        linux::net::SocketAddrExt,
        unix::{ffi::OsStrExt, net::SocketAddr},
    },
    path::PathBuf,
    pin::Pin,
    task::{Context, Poll},
};

use hickory_resolver::{TokioAsyncResolver, config::LookupIpStrategy, system_conf::parse_resolv_conf};
use mirrord_protocol::{
    RemoteError, RemoteResult, ResponseError,
    outgoing::{SocketAddress, UnixAddr},
};
use tokio::{
    fs,
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpStream, UnixStream},
};

use crate::util::path_resolver::InTargetPathResolver;

/// An enum that can mostly be used like tokio's [`TcpStream`] and [`UnixStream`], but can hold
/// either of them.
pub enum SocketStream {
    Ip(TcpStream),
    Unix(UnixStream),
}

impl From<TcpStream> for SocketStream {
    fn from(tcp_stream: TcpStream) -> Self {
        Self::Ip(tcp_stream)
    }
}

impl From<UnixStream> for SocketStream {
    fn from(unix_stream: UnixStream) -> Self {
        Self::Unix(unix_stream)
    }
}

impl SocketStream {
    /// Get the local address on this socket.
    pub fn local_addr(&self) -> io::Result<SocketAddress> {
        Ok(match self {
            SocketStream::Ip(tcp_stream) => SocketAddress::Ip(tcp_stream.local_addr()?),
            SocketStream::Unix(unix_stream) => {
                let local_address = SocketAddr::from(unix_stream.local_addr()?);

                let addr = if local_address.is_unnamed() {
                    UnixAddr::Unnamed
                } else if let Some(path) = local_address.as_pathname() {
                    UnixAddr::Pathname(path.to_path_buf())
                } else if let Some(name) = local_address.as_abstract_name() {
                    UnixAddr::Abstract(name.to_vec())
                } else {
                    return Err(io::Error::other("unknown unix address kind"));
                };

                SocketAddress::Unix(addr)
            }
        })
    }

    /// Resolve a hostname using the target pod's DNS configuration.
    ///
    /// This is used in multi-cluster scenarios where the same hostname resolves to different IPs
    /// in different clusters. Each agent resolves the hostname locally to connect to its own
    /// cluster's service.
    async fn resolve_hostname(hostname: &str, pid: Option<u64>, port: u16) -> RemoteResult<std::net::SocketAddr> {
        let etc_path = pid
            .map(|pid| PathBuf::from("/proc").join(pid.to_string()).join("root/etc"))
            .unwrap_or_else(|| PathBuf::from("/etc"));

        let resolv_conf_path = etc_path.join("resolv.conf");
        let resolv_conf = fs::read(&resolv_conf_path).await.map_err(|e| {
            ResponseError::Remote(RemoteError::DnsFailed(format!(
                "Failed to read resolv.conf: {}",
                e
            )))
        })?;

        let (config, mut options) = parse_resolv_conf(resolv_conf).map_err(|e| {
            ResponseError::Remote(RemoteError::DnsFailed(format!(
                "Failed to parse resolv.conf: {}",
                e
            )))
        })?;

        options.ip_strategy = LookupIpStrategy::Ipv4thenIpv6;

        let resolver = TokioAsyncResolver::tokio(config, options);

        let lookup = resolver.lookup_ip(hostname).await.map_err(|e| {
            ResponseError::Remote(RemoteError::DnsFailed(format!(
                "DNS lookup failed for {}: {}",
                hostname, e
            )))
        })?;

        let ip = lookup.iter().next().ok_or_else(|| {
            ResponseError::Remote(RemoteError::DnsFailed(format!(
                "No IP addresses found for {}",
                hostname
            )))
        })?;

        Ok(std::net::SocketAddr::new(ip, port))
    }

    /// Connect to a given [`SocketAddress`], whether IP or unix.
    ///
    /// If `hostname` is provided, the agent will resolve the hostname locally instead of using
    /// the IP in `addr`. This is essential for multi-cluster scenarios where the same hostname
    /// resolves to different IPs in different clusters.
    pub async fn connect(
        addr: SocketAddress,
        pid: Option<u64>,
        hostname: Option<&str>,
    ) -> RemoteResult<Self> {
        match addr {
            SocketAddress::Ip(original_addr) => {
                // If hostname is provided, resolve it locally instead of using the original IP.
                let addr = if let Some(hostname) = hostname {
                    tracing::debug!(
                        %hostname,
                        original = %original_addr,
                        "Re-resolving hostname locally for multi-cluster support"
                    );
                    Self::resolve_hostname(hostname, pid, original_addr.port()).await?
                } else {
                    original_addr
                };
                Ok(Self::from(TcpStream::connect(addr).await?))
            }
            SocketAddress::Unix(UnixAddr::Pathname(path)) => {
                // In order to connect to a unix socket on the target pod, instead of connecting to
                // /the/target/path we connect to /proc/<PID>/root/the/target/path.
                let path = if let Some(pid) = pid {
                    InTargetPathResolver::new(pid).resolve(&path)?
                } else {
                    path
                };

                Ok(Self::from(UnixStream::connect(path).await?))
            }
            SocketAddress::Unix(UnixAddr::Abstract(mut name)) => {
                // Abstract names are "paths" that start with a NUL byte.
                name.insert(0, 0);
                let path = OsStr::from_bytes(&name);
                Ok(Self::from(UnixStream::connect(path).await?))
            }
            SocketAddress::Unix(UnixAddr::Unnamed) => {
                // Cannot connect to unnamed socket by address.
                Err(ResponseError::Remote(RemoteError::InvalidAddress(addr)))
            }
        }
    }
}

impl AsyncRead for SocketStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            SocketStream::Ip(tcp_stream) => Pin::new(tcp_stream).poll_read(cx, buf),
            SocketStream::Unix(unix_stream) => Pin::new(unix_stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for SocketStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, Error>> {
        match self.get_mut() {
            SocketStream::Ip(tcp_stream) => Pin::new(tcp_stream).poll_write(cx, buf),
            SocketStream::Unix(unix_stream) => Pin::new(unix_stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        match self.get_mut() {
            SocketStream::Ip(tcp_stream) => Pin::new(tcp_stream).poll_flush(cx),
            SocketStream::Unix(unix_stream) => Pin::new(unix_stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        match self.get_mut() {
            SocketStream::Ip(tcp_stream) => Pin::new(tcp_stream).poll_shutdown(cx),
            SocketStream::Unix(unix_stream) => Pin::new(unix_stream).poll_shutdown(cx),
        }
    }
}
