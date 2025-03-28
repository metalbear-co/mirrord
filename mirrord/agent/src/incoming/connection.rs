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
///
/// Reading data from this handle automatically broadcasts the data
/// to all mirroring clients using a [`broadcast::Sender`]. This way the incoming data stream
/// throughput is limited only by the "real" reader (original destination or a stealing client).
/// Stalling mirroring clients do not affect the communication.
#[derive(Debug)]
pub struct RedirectedConnection {
    pub(super) source: SocketAddr,
    pub(super) destination: SocketAddr,
    pub(super) stream: TcpStream,
}

impl RedirectedConnection {
    pub fn source(&self) -> SocketAddr {
        self.source
    }

    pub fn destination(&self) -> SocketAddr {
        self.destination
    }

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
