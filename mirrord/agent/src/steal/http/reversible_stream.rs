use pin_project::pin_project;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    net::TcpStream,
    time,
    time::{Duration, Instant},
};
use tracing::{trace, warn};

use crate::steal::http::error::HttpTrafficError;

/// Wraps a [`TcpStream`] to allow a sort of _peek_ functionality, by reading the first bytes, but
/// then keeping them for later reads.
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

    /// We could be writing less then `HEADER_SIZE` bytes into `header`, in two cases:
    /// 1. Got a timeout before reading as `HEADER_SIZE` bytes.
    /// 2. Read an EOF after less than `HEADER_SIZE` bytes.
    /// So we need to always know how many bytes we actually have.
    header_len: usize,

    /// How many bytes out of the [`header`] were already read by the reader of this
    /// [`ReversibleStream`]. If the reader reads bytes into a buffer that is smaller than
    /// `HEADER_SIZE`, it would not read the whole [`header`] on the first read, so this is the
    /// amount of bytes that were already read. After all the bytes from the [`header`] were read,
    /// by the user of this struct, further reads are forwarded to the underlying `TcpStream`.
    num_forwarded: usize,
}

impl<const HEADER_SIZE: usize> ReversibleStream<HEADER_SIZE> {
    /// Build a Reversible stream from a TcpStream, move on if not done within given timeout.
    /// Return an Error if there was an error while reading from the TCP Stream.
    pub(crate) async fn read_header(
        stream: TcpStream,
        timeout: Duration,
    ) -> Result<Self, HttpTrafficError> {
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
                        warn!(
                            "Got early EOF after only {} bytes while creating reversible stream.",
                            this.header_len
                        );
                    } else {
                        trace!("\"Peeking\" into TCP stream start completed.")
                    }
                    break;
                }
                Ok(Ok(n)) => {
                    this.header_len += n;
                    trace!("Read {n} header bytes, {} total.", this.header_len);
                }
                Ok(Err(read_error)) => Err(read_error)?,
                Err(_elapsed) => {
                    warn!(
                        "Got timeout while trying to read first {HEADER_SIZE} bytes of TCP stream."
                    );
                    break;
                }
            }
        }
        debug_assert!(this.header_len <= HEADER_SIZE);
        trace!(
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
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.project();

        if this.num_forwarded < this.header_len {
            let leftover = &this.header[*this.num_forwarded..*this.header_len];

            let forward = leftover.get(..buf.remaining()).unwrap_or(leftover);
            buf.put_slice(forward);

            *this.num_forwarded += forward.len();

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
