use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use actix_codec::ReadBuf;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
};

/// A redirected TCP connection.
///
/// Allows for reading and writing data (implements [`AsyncRead`] and [`AsyncWrite`]).
#[derive(Debug)]
pub struct RedirectedConnection {
    pub(super) source: SocketAddr,
    pub(super) destination: SocketAddr,
    pub(super) stream: TcpStream,
}

impl RedirectedConnection {
    /// Returns the source of this connection (address of the remote TCP client).
    pub fn source(&self) -> SocketAddr {
        self.source
    }

    /// Returns the original destination of this connection.
    pub fn destination(&self) -> SocketAddr {
        self.destination
    }

    /// Returns the address of the TCP socket that accepted this connection.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.stream.local_addr()
    }
}

impl AsyncRead for RedirectedConnection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for RedirectedConnection {
    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.stream).poll_shutdown(cx)
    }

    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.stream).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        let this = self.get_mut();
        Pin::new(&mut this.stream).poll_write_vectored(cx, bufs)
    }
}
