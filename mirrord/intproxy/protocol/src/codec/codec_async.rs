use std::io::{self, ErrorKind};

use bincode::{Decode, Encode};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
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
    /// Decodes the next message from the underlying IO handler.
    /// Does not read any excessive bytes.
    ///
    /// This method is cancel safe. Callers select over it against other futures (the intproxy's
    /// `LayerConnection` races it against outgoing messages), so the returned future gets dropped
    /// whenever another branch wins. Progress through the current message is therefore kept in
    /// `self` rather than on the stack: dropping the future mid-message leaves the already
    /// consumed bytes recorded, and the next call resumes where this one stopped.
    ///
    /// Getting this wrong desynchronizes the stream rather than failing loudly - the bytes taken
    /// from the socket are gone, and the next call reads the middle of a message as a length
    /// prefix, which then either decodes into garbage or asks for an absurdly large allocation.
    pub async fn receive(&mut self) -> Result<Option<T>> {
        while self.prefix_read < PREFIX_BYTES {
            match self.reader.read(&mut self.prefix[self.prefix_read..]).await {
                // The peer is done sending. Treated as a clean end of the stream, matching what
                // `read_exact` reported here before.
                Ok(0) => return Ok(None),
                Ok(read) => self.prefix_read += read,
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
                Err(e) => Err(e)?,
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
            match self
                .reader
                .read(&mut self.buffer[self.payload_read..])
                .await
            {
                Ok(0) => Err(io::Error::from(ErrorKind::UnexpectedEof))?,
                Ok(read) => self.payload_read += read,
                Err(e) => Err(e)?,
            }
        }

        self.prefix_read = 0;
        self.payload_len = None;
        self.payload_read = 0;

        let value = bincode::decode_from_slice(&self.buffer, bincode::config::standard())?.0;

        Ok(Some(value))
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
