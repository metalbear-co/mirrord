use std::{
    io::{self, ErrorKind},
    pin::Pin,
    task::{Context, Poll, ready},
};

use bincode::{Decode, Encode};
use futures_core::Stream;
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::tcp::{OwnedReadHalf, OwnedWriteHalf},
};

use super::{BUFFER_SIZE, PREFIX_BYTES, Result};

/// Handles sending messages of type `T` through the underlying [AsyncWrite] of type `W`.
#[derive(Debug)]
pub struct AsyncEncoder<T, W> {
    buffer: Vec<u8>,
    writer: W,
    _phantom: std::marker::PhantomData<fn() -> T>,
}

impl<T, W> AsyncEncoder<T, W> {
    /// Wraps the underlying IO handler.
    pub fn new(writer: W) -> Self {
        Self {
            buffer: Vec::with_capacity(BUFFER_SIZE),
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
    /// Encodes the given value into the inner IO handler.
    pub async fn send(&mut self, value: &T) -> Result<()> {
        self.buffer.resize(PREFIX_BYTES, 0);
        let bytes: u32 =
            bincode::encode_into_std_write(value, &mut self.buffer, bincode::config::standard())?
                .try_into()?;
        self.buffer
            .get_mut(..PREFIX_BYTES)
            .expect("buffer to short")
            .copy_from_slice(&bytes.to_be_bytes());

        self.writer.write_all(&self.buffer).await?;

        Ok(())
    }

    /// Flushes the inner IO handler.
    pub async fn flush(&mut self) -> Result<()> {
        self.writer.flush().await.map_err(Into::into)
    }
}

/// Handles receiving messages of type `T` from the underlying [AsyncRead] of type `W`.
#[derive(Debug)]
pub struct AsyncDecoder<T, R> {
    buffer: Vec<u8>,
    reader: R,
    /// Length prefix bytes read so far for the message currently being decoded.
    ///
    /// Retained across [`AsyncDecoder::receive`] calls to keep it cancel safe.
    prefix: [u8; PREFIX_BYTES],
    /// How much of [`AsyncDecoder::prefix`] is filled.
    prefix_read: usize,
    /// Payload length, known once the whole prefix has been read.
    payload_len: Option<usize>,
    /// How much of the payload was already read into [`AsyncDecoder::buffer`].
    payload_read: usize,
    _phantom: std::marker::PhantomData<fn() -> T>,
}

impl<T, R> AsyncDecoder<T, R> {
    /// Wraps the underlying IO handler.
    pub fn new(reader: R) -> Self {
        Self {
            buffer: Vec::with_capacity(BUFFER_SIZE),
            reader,
            prefix: [0; PREFIX_BYTES],
            prefix_read: 0,
            payload_len: None,
            payload_read: 0,
            _phantom: Default::default(),
        }
    }

    /// Unwraps the underlying IO handler.
    pub fn into_inner(self) -> R {
        self.reader
    }
}

impl<T, R> AsyncDecoder<T, R>
where
    T: Decode<()>,
    R: AsyncRead + Unpin,
{
    /// Polls for the next message from the underlying IO handler.
    /// Does not read any excessive bytes.
    ///
    /// Returning [`Poll::Pending`] part-way through a message is the normal case, so all progress
    /// lives in `self`. A caller that stops polling - a `select!` branch that loses the race, and
    /// so gets its future dropped - can resume with a later call without losing the bytes already
    /// taken from the reader.
    ///
    /// Losing them does not fail loudly, it desynchronizes the stream: the next read takes the
    /// middle of a message for a length prefix, which then either decodes into garbage or asks for
    /// an absurdly large allocation. Keeping the state machine behind a `poll` signature is what
    /// makes that unrepresentable, since nothing can be held across a [`Poll::Pending`] return.
    pub fn poll_receive(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<T>>> {
        while self.prefix_read < PREFIX_BYTES {
            let mut buf = ReadBuf::new(&mut self.prefix[self.prefix_read..]);
            ready!(Pin::new(&mut self.reader).poll_read(cx, &mut buf))?;

            match buf.filled().len() {
                // The peer is done sending. Treated as a clean end of the stream, matching what
                // `read_exact` reported here before.
                0 => return Poll::Ready(Ok(None)),
                read => self.prefix_read += read,
            }
        }

        let len = match self.payload_len {
            Some(len) => len,
            None => {
                let len = u32::from_be_bytes(self.prefix) as usize;
                self.buffer.resize(len, 0);
                self.payload_read = 0;
                self.payload_len = Some(len);
                len
            }
        };

        while self.payload_read < len {
            let mut buf = ReadBuf::new(&mut self.buffer[self.payload_read..]);
            ready!(Pin::new(&mut self.reader).poll_read(cx, &mut buf))?;

            match buf.filled().len() {
                0 => Err(io::Error::from(ErrorKind::UnexpectedEof))?,
                read => self.payload_read += read,
            }
        }

        self.prefix_read = 0;
        self.payload_len = None;
        self.payload_read = 0;

        let value = bincode::decode_from_slice(&self.buffer, bincode::config::standard())?.0;

        Poll::Ready(Ok(Some(value)))
    }

    /// Decodes the next message from the underlying IO handler.
    /// Does not read any excessive bytes.
    ///
    /// Cancel safe, see [`AsyncDecoder::poll_receive`].
    pub async fn receive(&mut self) -> Result<Option<T>> {
        std::future::poll_fn(|cx| self.poll_receive(cx)).await
    }
}

impl<T, R> Stream for AsyncDecoder<T, R>
where
    T: Decode<()>,
    R: AsyncRead + Unpin,
{
    type Item = Result<T>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match ready!(self.get_mut().poll_receive(cx)) {
            Ok(Some(value)) => Poll::Ready(Some(Ok(value))),
            Ok(None) => Poll::Ready(None),
            Err(error) => Poll::Ready(Some(Err(error))),
        }
    }
}

/// Creates a new pair of [`AsyncEncoder`] and [`AsyncDecoder`], using the given asynchronous
/// [`TcpStream`](tokio::net::TcpStream).
pub fn make_async_framed<T1: Encode, T2: Decode<()>>(
    stream: tokio::net::TcpStream,
) -> (
    AsyncEncoder<T1, OwnedWriteHalf>,
    AsyncDecoder<T2, OwnedReadHalf>,
) {
    let (reader, writer) = stream.into_split();

    let sender = AsyncEncoder::new(writer);
    let receiver = AsyncDecoder::new(reader);

    (sender, receiver)
}
