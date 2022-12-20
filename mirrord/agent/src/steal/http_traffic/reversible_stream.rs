use core::ops::Deref;
use std::net::SocketAddr;

use pin_project::pin_project;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    net::TcpStream,
};

use super::error::HttpTrafficError;

/// Wraps a [`TcpStream`] to allow a sort of _peek_ functionality, even though we're actually going
/// to be calling [`TcpStream::read_exact`] on it.
///
/// Very useful to the HTTP filter component on [`stealer`], where we have to look at the first
/// message on a [`TcpStream`] to try and identify if this connection is _talking_ HTTP.
///
/// Thanks [finomnis](https://stackoverflow.com/users/2902833/finomnis) for the help!
// impl deref with pin
#[derive(Debug)]
#[pin_project]
pub(crate) struct ReversibleStream<const HEADER_SIZE: usize> {
    #[pin]
    stream: TcpStream,
    header: [u8; HEADER_SIZE],
    num_forwarded: usize,
}

impl<const HEADER_SIZE: usize> ReversibleStream<HEADER_SIZE> {
    pub(crate) async fn read_header(stream: TcpStream) -> Result<Self, HttpTrafficError> {
        let mut this = Self {
            stream,
            header: [0; HEADER_SIZE],
            num_forwarded: 0,
        };

        this.stream.read_exact(&mut this.header).await?;

        Ok(this)
    }

    pub(crate) fn get_header(&mut self) -> &[u8; HEADER_SIZE] {
        &self.header
    }

    pub(crate) fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.stream.local_addr()
    }
}

impl<const HEADER_SIZE: usize> AsyncRead for ReversibleStream<HEADER_SIZE> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.project();

        if *this.num_forwarded < this.header.len() {
            let leftover = &this.header[*this.num_forwarded..];

            let num_forward_now = leftover.len().min(buf.remaining());
            let forward = &leftover[..num_forward_now];
            buf.put_slice(forward);

            *this.num_forwarded += num_forward_now;

            std::task::Poll::Ready(Ok(()))
        } else {
            this.stream.poll_read(cx, buf)
        }
    }
}

impl<const HEADER_SIZE: usize> AsyncWrite for ReversibleStream<HEADER_SIZE> {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.project().stream.poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.project().stream.poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.project().stream.poll_shutdown(cx)
    }
}
