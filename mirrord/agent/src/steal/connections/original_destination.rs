use std::{
    io::{IoSlice, Result},
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use actix_codec::ReadBuf;
use http::Uri;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
};
use tokio_rustls::client::TlsStream;

use crate::steal::tls::handler::PassThroughTlsConnector;

/// Established connection with the original destination server.
pub enum MaybeTls {
    NoTls(TcpStream),
    /// Wrapped in [`Box`] due to big size.
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for MaybeTls {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        match self.get_mut() {
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
            Self::NoTls(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeTls {
    fn is_write_vectored(&self) -> bool {
        match self {
            Self::Tls(stream) => stream.is_write_vectored(),
            Self::NoTls(stream) => stream.is_write_vectored(),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        match self.get_mut() {
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
            Self::NoTls(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        match self.get_mut() {
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
            Self::NoTls(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }

    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize>> {
        match self.get_mut() {
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
            Self::NoTls(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<Result<usize>> {
        match self.get_mut() {
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_write_vectored(cx, bufs),
            Self::NoTls(stream) => Pin::new(stream).poll_write_vectored(cx, bufs),
        }
    }
}

/// Original destination of a stolen HTTPS request.
///
/// Used in [`FilteredStealTask`](super::filtered::FilteredStealTask).
#[derive(Clone, Debug)]
pub struct OriginalDestination {
    address: SocketAddr,
    connector: Option<PassThroughTlsConnector>,
}

impl OriginalDestination {
    /// Creates a new instance.
    ///
    /// # Params
    ///
    /// * `address` - address of the HTTP server.
    /// * `connector` - optional TLS connector. If given, it means that the original HTTP connection
    ///   was wrapped in TLS. We should pass the requests with TLS as well.
    pub fn new(address: SocketAddr, connector: Option<PassThroughTlsConnector>) -> Self {
        Self { address, connector }
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Makes a connection to the server.
    ///
    /// Given [`Uri`] will be used in case we need a TLS connection and we don't have the original
    /// SNI (we need some server name to connect with TLS).
    pub async fn connect(&self, request_uri: &Uri) -> Result<MaybeTls> {
        let stream = TcpStream::connect(self.address).await?;

        match self.connector.as_ref() {
            Some(connector) => {
                let stream = connector
                    .connect(self.address.ip(), Some(request_uri), stream)
                    .await?;
                Ok(MaybeTls::Tls(stream))
            }
            None => Ok(MaybeTls::NoTls(stream)),
        }
    }
}
