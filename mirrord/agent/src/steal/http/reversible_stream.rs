use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    net::TcpStream,
    time,
    time::{Duration, Instant},
};

/// Wraps a [`TcpStream`] to allow a sort of _peek_ functionality, by reading the first bytes, but
/// then keeping them for later reads.
///
/// Very useful to the HTTP filter component on `stealer`, where we have to look at the first
/// message on a [`TcpStream`] to try and identify if this connection is _talking_ HTTP.
///
/// Thanks [finomnis](https://stackoverflow.com/users/2902833/finomnis) for the help!
// impl deref with pin
#[derive(Debug)]
pub(crate) struct ReversibleStream<const HEADER_SIZE: usize> {
    stream: TcpStream,

    header: [u8; HEADER_SIZE],

    /// We could be writing less then `HEADER_SIZE` bytes into `header`, in two cases:
    /// 1. Got a timeout before reading as `HEADER_SIZE` bytes.
    /// 2. Read an EOF after less than `HEADER_SIZE` bytes.
    /// So we need to always know how many bytes we actually have.
    header_len: usize,

    /// How many bytes out of the `header` were already read by the reader of this
    /// [`ReversibleStream`]. If the reader reads bytes into a buffer that is smaller than
    /// `HEADER_SIZE`, it would not read the whole `header` on the first read, so this is the
    /// amount of bytes that were already read. After all the bytes from the `header` were read,
    /// by the user of this struct, further reads are forwarded to the underlying `TcpStream`.
    num_forwarded: usize,
}

impl<const HEADER_SIZE: usize> ReversibleStream<HEADER_SIZE> {
    /// Build a [`ReversibleStream`] from a [`TcpStream`], move on if not done within given timeout.
    /// Return an Error if there was an error while reading from the [`TcpStream`].
    pub(crate) async fn read_header(stream: TcpStream, timeout: Duration) -> io::Result<Self> {
        let mut this = Self {
            stream,
            header: [0; HEADER_SIZE],
            header_len: 0,
            num_forwarded: 0,
        };

        let mut mut_buf = &mut this.header[..];

        let deadline = Instant::now() + timeout;

        // I think we can't use `stream.take(...).read_to_end(...)` because `take` takes ownership
        // of the stream and there is no `by_ref()` for `AsyncRead`.
        // And I think we can't use `read_exact()` because that loses track of the read bytes in the
        // case of an early EOF.
        //
        // Read bytes from the stream until the buffer is full, or until an EOF.
        loop {
            match time::timeout_at(deadline, this.stream.read_buf(&mut mut_buf)).await {
                Ok(Ok(0)) => {
                    if this.header_len != HEADER_SIZE {
                        tracing::trace!(
                            "Got early EOF after only {} bytes while creating reversible stream.",
                            this.header_len
                        );
                    } else {
                        tracing::trace!("\"Peeking\" into TCP stream start completed.")
                    }
                    break;
                }
                Ok(Ok(n)) => {
                    this.header_len += n;
                    tracing::trace!("Read {n} header bytes, {} total.", this.header_len);
                }
                Ok(Err(read_error)) => Err(read_error)?,
                Err(_elapsed) => {
                    tracing::trace!(
                        "Got timeout while trying to read first {HEADER_SIZE} bytes of TCP stream."
                    );
                    break;
                }
            }
        }
        debug_assert!(this.header_len <= HEADER_SIZE);
        tracing::trace!(
            "created reversible stream with header: {}",
            String::from_utf8_lossy(&this.header)
        );
        Ok(this)
    }

    pub(crate) fn get_header(&mut self) -> &[u8] {
        &self.header[..self.header_len]
    }
}

impl<const HEADER_SIZE: usize> AsyncRead for ReversibleStream<HEADER_SIZE> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.num_forwarded < self.header_len {
            let leftover = &self.header[self.num_forwarded..self.header_len];

            let forward: &[u8] = leftover.get(..buf.remaining()).unwrap_or(leftover);
            buf.put_slice(forward);

            self.get_mut().num_forwarded += forward.len();

            Poll::Ready(Ok(()))
        } else {
            Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
        }
    }
}

impl<const HEADER_SIZE: usize> AsyncWrite for ReversibleStream<HEADER_SIZE> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}
