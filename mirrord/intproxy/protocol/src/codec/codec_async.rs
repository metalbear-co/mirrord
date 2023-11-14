use std::io::ErrorKind;

use bincode::{Decode, Encode};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::tcp::{OwnedReadHalf, OwnedWriteHalf},
};

use super::{Result, BUFFER_SIZE, PREFIX_BYTES};

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
    _phantom: std::marker::PhantomData<fn() -> T>,
}

impl<T, R> AsyncDecoder<T, R> {
    /// Wraps the underlying IO handler.
    pub fn new(reader: R) -> Self {
        Self {
            buffer: Vec::with_capacity(BUFFER_SIZE),
            reader,
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
    T: Decode,
    R: AsyncRead + Unpin,
{
    /// Decodes the next message from the underlying IO handler.
    /// Does not read any excessive bytes.
    pub async fn receive(&mut self) -> Result<Option<T>> {
        let mut len_buffer = [0; 4];
        match self.reader.read_exact(&mut len_buffer).await {
            Ok(..) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => Err(e)?,
        }
        let len = u32::from_be_bytes(len_buffer);

        self.buffer.resize(len as usize, 0);
        self.reader.read_exact(&mut self.buffer).await?;

        let value = bincode::decode_from_slice(&self.buffer, bincode::config::standard())?.0;

        Ok(Some(value))
    }
}

/// Creates a new pair of [`AsyncEncoder`] and [`AsyncDecoder`], using the given asynchronous
/// [`TcpStream`](tokio::net::TcpStream).
pub fn make_async_framed<T1: Encode, T2: Decode>(
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
