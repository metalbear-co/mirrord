use std::{
    io,
    ops::Not,
    pin::Pin,
    task::{Context, Poll},
};

use actix_codec::ReadBuf;
use bytes::Buf;
use tokio::io::{AsyncRead, AsyncWrite};

/// A wrapper over an IO stream with a prepended prefix.
///
/// Its [`AsyncRead`] implementation will first read from the prefix, and only when it is
/// exhausted, it will read from the inner stream.
///
/// Once the prefix is exhausted, it is dropped to free the memory.
pub struct RolledBackStream<IO, B> {
    stream: IO,
    prefix: Option<B>,
}

impl<IO, B> RolledBackStream<IO, B>
where
    B: Buf,
{
    /// Prepends the given stream with the `prefix`.
    pub fn new(stream: IO, prefix: B) -> Self {
        Self {
            stream,
            prefix: prefix.has_remaining().then_some(prefix),
        }
    }
}

impl<IO, B> RolledBackStream<IO, B> {
    /// Returns a reference to the inner IO stream.
    pub fn inner(&self) -> &IO {
        &self.stream
    }
}

impl<IO, B> AsyncRead for RolledBackStream<IO, B>
where
    IO: AsyncRead + Unpin,
    B: Buf + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        let Some(prefix) = this.prefix.as_mut() else {
            return Pin::new(&mut this.stream).poll_read(cx, buf);
        };

        // At this point, `prefix` cannot be empty.
        // This is guaranteed by the check in `RolledBackStream::new`
        // and the check at the end of this function.

        let mut remaining_capacity = buf.remaining();
        while remaining_capacity > 0 {
            let chunk = prefix.chunk();

            if chunk.is_empty() {
                break;
            }

            let chunk = chunk.get(..remaining_capacity).unwrap_or(chunk);
            buf.put_slice(chunk);
            prefix.advance(chunk.len());
            remaining_capacity = buf.remaining();
        }

        if prefix.has_remaining().not() {
            this.prefix = None;
        }

        return Poll::Ready(Ok(()));
    }
}

impl<IO, B> AsyncWrite for RolledBackStream<IO, B>
where
    IO: AsyncWrite + Unpin,
    B: Unpin,
{
    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }

    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<Result<usize, io::Error>> {
        Pin::new(&mut self.get_mut().stream).poll_write_vectored(cx, bufs)
    }
}

#[cfg(test)]
mod test {
    use rstest::rstest;
    use tokio::io::AsyncReadExt;

    use super::RolledBackStream;

    #[rstest]
    #[case(
        "some data",
        "some more data",
        &[
            "some dat",
            "a",
            "some mor",
            "e data",
        ],
    )]
    #[case(
        "GET / HTTP/1.1\r\n\r\n",
        "",
        &[
            "GET / HT",
            "TP/1.1\r\n",
            "\r\n",
        ],
    )]
    #[tokio::test]
    async fn rolledback_stream_read(
        #[case] buffer: &str,
        #[case] stream: &str,
        #[case] expected_reads: &[&str],
    ) {
        let mut stream = RolledBackStream::new(stream.as_bytes(), buffer.as_bytes());

        for expected_read in expected_reads {
            let mut buffer = [0_u8; 8];
            let bytes_read = stream.read_buf(&mut buffer.as_mut_slice()).await.unwrap();
            assert_eq!(
                std::str::from_utf8(buffer.get(..bytes_read).unwrap()).unwrap(),
                *expected_read,
            );
        }
    }
}
