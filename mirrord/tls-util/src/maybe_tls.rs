use std::{
    io::{IoSlice, Result},
    pin::Pin,
    task::{Context, Poll},
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
};
use tokio_rustls::client::TlsStream;

/// Either a TCP connection or a TLS client connection over TCP.
pub enum MaybeTls {
    /// Plain TCP connection.
    NoTls(TcpStream),
    /// TCP wrapped in TLS.
    ///
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

impl AsRef<TcpStream> for MaybeTls {
    fn as_ref(&self) -> &TcpStream {
        match self {
            Self::NoTls(stream) => stream,
            Self::Tls(stream) => stream.get_ref().0,
        }
    }
}
