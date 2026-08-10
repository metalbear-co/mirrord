use std::{
    io,
    ops::Not,
    pin::Pin,
    task::{Context, Poll},
};

use bincode::{
    BorrowDecode, Encode,
    enc::{EncoderImpl, write::SizeWriter},
};
use bytes::{Buf, BufMut, BytesMut, buf::Writer};
use futures::{Sink, Stream};
use mirrord_protocol::payload::FullData;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::tcp::{OwnedReadHalf, OwnedWriteHalf},
};

use super::{BUFFER_SIZE, PREFIX_BYTES, Result};
use crate::codec::CodecError;

/// Handles sending messages of type `T` through the underlying [AsyncWrite] of type `W`.
///
/// Implements [`Sink`].
#[derive(Debug)]
pub struct AsyncEncoder<T, W> {
    buffer: Writer<BytesMut>,
    writer: W,
    _phantom: std::marker::PhantomData<fn() -> T>,
}

impl<T, W> AsyncEncoder<T, W> {
    /// Wraps the underlying IO handler.
    pub fn new(writer: W) -> Self {
        Self {
            buffer: BytesMut::with_capacity(BUFFER_SIZE).writer(),
            writer,
            _phantom: Default::default(),
        }
    }

    /// Unwraps the underlying IO handler.
    pub fn into_inner(self) -> W {
        self.writer
    }
}

impl<T, W> AsyncEncoder<T, W>
where
    T: Encode,
    W: AsyncWrite + Unpin,
{
    fn poll_write(&mut self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        while self.buffer.get_ref().is_empty().not() {
            let written = std::task::ready!(
                Pin::new(&mut self.writer).poll_write(cx, self.buffer.get_ref())
            )?;
            if written == 0 {
                return Poll::Ready(Err(CodecError::IoError(io::ErrorKind::WriteZero.into())));
            }
            self.buffer.get_mut().advance(written);
        }
        Poll::Ready(Ok(()))
    }
}

impl<T, W> Sink<T> for AsyncEncoder<T, W>
where
    T: Encode,
    W: AsyncWrite + Unpin,
{
    type Error = CodecError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        self.get_mut().poll_write(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: T) -> Result<()> {
        let this = self.get_mut();
        if this.buffer.get_ref().is_empty().not() {
            return Err(CodecError::IoError(io::Error::other("codec not ready")));
        }

        let size = {
            let mut size_writer =
                EncoderImpl::<_, _>::new(SizeWriter::default(), bincode::config::standard());
            item.encode(&mut size_writer)?;
            size_writer.into_writer().bytes_written
        };
        let size_u32 = u32::try_from(size)?;
        let total_size = size + PREFIX_BYTES;
        // This reclaims freed capacity if possible.
        this.buffer.get_mut().reserve(total_size);

        this.buffer.get_mut().put_u32(size_u32);
        bincode::encode_into_std_write(item, &mut this.buffer, bincode::config::standard())?;

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let this = self.get_mut();
        std::task::ready!(this.poll_write(cx))?;
        Pin::new(&mut this.writer)
            .poll_flush(cx)
            .map_err(CodecError::IoError)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let this = self.get_mut();
        std::task::ready!(this.poll_write(cx))?;
        Pin::new(&mut this.writer)
            .poll_shutdown(cx)
            .map_err(CodecError::IoError)
    }
}

/// Handles receiving messages of type `T` from the underlying [AsyncRead] of type `W`.
///
/// Implements [`Stream`].
#[derive(Debug)]
pub struct AsyncDecoder<T, R> {
    buffer: BytesMut,
    state: DecoderState,
    reader: R,
    _phantom: std::marker::PhantomData<fn() -> T>,
}

impl<T, R> AsyncDecoder<T, R> {
    /// Wraps the underlying IO handler.
    pub fn new(reader: R) -> Self {
        Self {
            buffer: BytesMut::with_capacity(BUFFER_SIZE),
            state: DecoderState::ReadingPrefix {
                buffer: Default::default(),
                filled: 0,
            },
            reader,
            _phantom: Default::default(),
        }
    }

    /// Unwraps the underlying IO handler.
    pub fn into_inner(self) -> R {
        self.reader
    }
}

#[derive(Debug)]
enum DecoderState {
    ReadingPrefix {
        buffer: [u8; PREFIX_BYTES],
        filled: usize,
    },
    ReadingMessage {
        message_len: usize,
    },
}

impl<T, R> Stream for AsyncDecoder<T, R>
where
    T: for<'de> BorrowDecode<'de, FullData>,
    R: AsyncRead + Unpin,
{
    type Item = Result<T>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match &mut this.state {
                DecoderState::ReadingPrefix { buffer, filled } => {
                    let missing = buffer
                        .get_mut(*filled..)
                        .expect("filled counter should not exceed the buffer size");
                    let mut read_buf = ReadBuf::new(missing);
                    std::task::ready!(Pin::new(&mut this.reader).poll_read(cx, &mut read_buf))?;
                    let filled_now = read_buf.filled().len();
                    if filled_now == 0 {
                        let result = if *filled == 0 {
                            None
                        } else {
                            Some(Err(CodecError::IoError(
                                io::ErrorKind::UnexpectedEof.into(),
                            )))
                        };
                        break Poll::Ready(result);
                    }
                    *filled += filled_now;
                    if *filled == buffer.len() {
                        let message_len = usize::try_from(u32::from_be_bytes(*buffer))?;
                        this.state = DecoderState::ReadingMessage { message_len };
                        this.buffer.reserve(message_len);
                    }
                }

                DecoderState::ReadingMessage { message_len }
                    if this.buffer.len() < *message_len =>
                {
                    let missing = *message_len - this.buffer.len();
                    let mut limited = (&mut this.buffer).limit(missing);
                    let bytes_read = std::task::ready!(tokio_util::io::poll_read_buf(
                        Pin::new(&mut this.reader),
                        cx,
                        &mut limited
                    ))?;
                    if bytes_read == 0 {
                        break Poll::Ready(Some(Err(CodecError::IoError(
                            io::ErrorKind::UnexpectedEof.into(),
                        ))));
                    }
                }

                DecoderState::ReadingMessage { .. } => {
                    let data = this.buffer.split().freeze();
                    let context = FullData(Some(data.clone()));
                    let (value, consumed) =
                        bincode::borrow_decode_from_slice_with_context::<_, T, _>(
                            data.as_ref(),
                            bincode::config::standard(),
                            context,
                        )?;
                    if consumed < data.len() {
                        break Poll::Ready(Some(Err(CodecError::IoError(io::Error::other(
                            "detected leftover bytes",
                        )))));
                    }
                    this.state = DecoderState::ReadingPrefix {
                        buffer: Default::default(),
                        filled: 0,
                    };
                    break Poll::Ready(Some(Ok(value)));
                }
            }
        }
    }
}

/// Creates a new pair of [`AsyncEncoder`] and [`AsyncDecoder`], using the given asynchronous
/// [`TcpStream`](tokio::net::TcpStream).
pub fn make_async_framed<T1, T2>(
    stream: tokio::net::TcpStream,
) -> (
    AsyncEncoder<T1, OwnedWriteHalf>,
    AsyncDecoder<T2, OwnedReadHalf>,
)
where
    T1: Encode,
    T2: for<'de> BorrowDecode<'de, FullData>,
{
    let (reader, writer) = stream.into_split();

    let sender = AsyncEncoder::new(writer);
    let receiver = AsyncDecoder::new(reader);

    (sender, receiver)
}

#[cfg(test)]
mod test {
    use futures::{SinkExt, StreamExt, TryStreamExt};

    use crate::codec::{AsyncDecoder, AsyncEncoder};

    #[tokio::test]
    async fn encode_decode() {
        let (reader, writer) = tokio::io::simplex(1);
        let decoder = AsyncDecoder::<String, _>::new(reader);
        let mut encoder = AsyncEncoder::<String, _>::new(writer);

        let messages = ["hello", "from", "the", "other", "side", ""]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();

        let (received, ()) = tokio::join!(decoder.try_collect::<Vec<_>>(), async {
            let mut messages = futures::stream::iter(&messages).map(Clone::clone).map(Ok);
            encoder.send_all(&mut messages).await.unwrap();
            encoder.close().await.unwrap();
        },);

        assert_eq!(received.unwrap(), messages,);
    }
}
