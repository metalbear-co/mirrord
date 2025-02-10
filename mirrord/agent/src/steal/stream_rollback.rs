use std::{
    io::{Cursor, Result},
    pin::Pin,
    task::{Context, Poll},
};

use actix_codec::ReadBuf;
use bytes::Buf;
use tokio::io::{AsyncRead, AsyncWrite};

pub struct StreamWithRollback<const CAN_ROLLBACK: bool, T> {
    inner: T,
    buffer: Cursor<Vec<u8>>,
}

impl<T> StreamWithRollback<true, T> {
    pub const DEFAULT_CAPACITY: usize = 1024 * 4;

    pub fn new(inner: T, capacity: usize) -> Self {
        Self {
            inner,
            buffer: Cursor::new(Vec::with_capacity(capacity)),
        }
    }

    pub fn rollback(&mut self) {
        self.buffer.set_position(0);
    }

    pub fn disable_rollback(self) -> StreamWithRollback<false, T> {
        let mut result: StreamWithRollback<false, T> = StreamWithRollback {
            inner: self.inner,
            buffer: self.buffer,
        };

        result.try_drop_buffer();

        result
    }
}

impl<T> StreamWithRollback<false, T> {
    fn try_drop_buffer(&mut self) {
        if !self.buffer.has_remaining() {
            self.buffer = Default::default();
        }
    }
}

impl<T> AsyncRead for StreamWithRollback<true, T>
where
    T: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        let to_fill = buf.remaining();
        if to_fill == 0 {
            return Poll::Ready(Ok(()));
        }

        let this = self.get_mut();

        if this.buffer.has_remaining() {
            let mut chunk = this.buffer.chunk();
            chunk = chunk.get(..to_fill).unwrap_or(chunk);
            buf.put_slice(chunk);
            this.buffer.advance(chunk.len());
            return Poll::Ready(Ok(()));
        }

        let inner = Pin::new(&mut this.inner);
        let result = inner.poll_read(cx, buf);

        if matches!(result, Poll::Ready(Ok(()))) {
            let bytes_read_now = to_fill - buf.remaining();
            let total_filled = buf.filled().len();
            let new_bytes = buf
                .filled()
                .get(total_filled - bytes_read_now..)
                .expect("detected a bug in async read implementation");
            this.buffer.get_mut().extend_from_slice(new_bytes);
            this.buffer.advance(new_bytes.len());
        }

        result
    }
}

impl<T> AsyncRead for StreamWithRollback<false, T>
where
    T: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        let to_fill = buf.remaining();
        if to_fill == 0 {
            return Poll::Ready(Ok(()));
        }

        let this = self.get_mut();

        if this.buffer.has_remaining() {
            let mut chunk = this.buffer.chunk();
            chunk = chunk.get(..to_fill).unwrap_or(chunk);
            buf.put_slice(chunk);
            this.buffer.advance(chunk.len());

            this.try_drop_buffer();

            return Poll::Ready(Ok(()));
        }

        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<const CAN_ROLLBACK: bool, T> AsyncWrite for StreamWithRollback<CAN_ROLLBACK, T>
where
    T: AsyncWrite + Unpin,
{
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }

    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }
}

#[cfg(test)]
mod test {
    use std::net::{Ipv4Addr, SocketAddr};

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    use crate::steal::stream_rollback::StreamWithRollback;

    #[tokio::test]
    async fn simple_rollback_on_tcp_stream() {
        let listener = TcpListener::bind(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let mut stream = TcpStream::connect(address).await.unwrap();
            stream.write_all(b"hello there\n").await.unwrap();

            stream.write_all(b"hello there again\n").await.unwrap();

            stream.write_all(b"and again\n").await.unwrap();

            stream.shutdown().await.unwrap();
        });

        let stream = listener.accept().await.unwrap().0;
        let mut with_rollback = StreamWithRollback::new(stream, 64);

        let mut data = String::with_capacity(64);
        with_rollback.read_to_string(&mut data).await.unwrap();
        assert_eq!(data, "hello there\nhello there again\nand again\n");

        with_rollback.rollback();

        let mut data = String::with_capacity(64);
        with_rollback.read_to_string(&mut data).await.unwrap();
        assert_eq!(data, "hello there\nhello there again\nand again\n");
    }
}
