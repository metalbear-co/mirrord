use std::{
    io::{IoSlice, Result},
    pin::Pin,
    task::{Context, Poll},
};

use actix_codec::ReadBuf;
use mirrord_tls_util::tokio_rustls::client::TlsStream;
use tokio::io::{AsyncRead, AsyncWrite};

pub enum MaybeTls<IO> {
    /// Wrapped in [`Box`], because clippy complains about variants size difference.
    Tls(Box<TlsStream<IO>>),
    NoTls(IO),
}

impl<IO> AsyncRead for MaybeTls<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
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

impl<IO> AsyncWrite for MaybeTls<IO>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
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
