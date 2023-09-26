use std::{
    io::{self, ErrorKind, Read, Write},
    num::TryFromIntError,
};

use bincode::{
    error::{DecodeError, EncodeError},
    Decode, Encode,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::tcp::{OwnedReadHalf, OwnedWriteHalf},
};

#[derive(Error, Debug)]
pub enum CodecError {
    #[error("encoding failed: {0}")]
    EncodingFailed(#[from] EncodeError),
    #[error("decoding failed: {0}")]
    DecodingFailed(#[from] DecodeError),
    #[error("message too long: {0}")]
    MessageTooLong(#[from] TryFromIntError),
    #[error("io failed: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = core::result::Result<T, CodecError>;

const BUFFER_SIZE: usize = 1024;
const PREFIX_BYTES: usize = u32::BITS as usize / 8;

#[derive(Debug)]
pub struct SyncSender<T, W> {
    buffer: Vec<u8>,
    writer: W,
    _phantom: std::marker::PhantomData<T>,
}

impl<T, W> SyncSender<T, W> {
    pub fn new(writer: W) -> Self {
        Self {
            buffer: Vec::with_capacity(BUFFER_SIZE),
            writer,
            _phantom: Default::default(),
        }
    }
}

impl<T, W> SyncSender<T, W>
where
    T: Encode,
    W: Write,
{
    pub fn send(&mut self, value: &T) -> Result<()> {
        self.buffer.resize(PREFIX_BYTES, 0);
        let bytes: u32 =
            bincode::encode_into_std_write(value, &mut self.buffer, bincode::config::standard())?
                .try_into()?;
        self.buffer[..PREFIX_BYTES].copy_from_slice(&bytes.to_be_bytes());

        self.writer.write_all(&self.buffer)?;

        Ok(())
    }
}

#[derive(Debug)]
pub struct SyncReceiver<T, R> {
    buffer: Vec<u8>,
    reader: R,
    _phantom: std::marker::PhantomData<T>,
}

impl<T, R> SyncReceiver<T, R> {
    pub fn new(reader: R) -> Self {
        Self {
            buffer: Vec::with_capacity(BUFFER_SIZE),
            reader,
            _phantom: Default::default(),
        }
    }
}

impl<T, R> SyncReceiver<T, R>
where
    T: Decode,
    R: Read,
{
    pub fn receive(&mut self) -> Result<Option<T>> {
        let mut len_buffer = [0; 4];
        match self.reader.read_exact(&mut len_buffer) {
            Ok(..) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => Err(e)?,
        }
        let len = u32::from_be_bytes(len_buffer);

        self.buffer.resize(len as usize, 0);
        self.reader.read_exact(&mut self.buffer)?;

        let value = bincode::decode_from_slice(&self.buffer, bincode::config::standard())?.0;

        Ok(Some(value))
    }
}

pub fn make_sync_framed<T1: Encode, T2: Decode>(
    stream: std::net::TcpStream,
) -> Result<(
    SyncSender<T1, std::net::TcpStream>,
    SyncReceiver<T2, std::net::TcpStream>,
)> {
    let stream_cloned = stream.try_clone()?;

    let sender = SyncSender::new(stream);
    let receiver = SyncReceiver::new(stream_cloned);

    Ok((sender, receiver))
}

#[derive(Debug)]
pub struct AsyncSender<T, W> {
    buffer: Vec<u8>,
    writer: W,
    _phantom: std::marker::PhantomData<T>,
}

impl<T, W> AsyncSender<T, W> {
    pub fn new(writer: W) -> Self {
        Self {
            buffer: Vec::with_capacity(BUFFER_SIZE),
            writer,
            _phantom: Default::default(),
        }
    }
}

impl<T, W> AsyncSender<T, W>
where
    T: Encode,
    W: AsyncWrite + Unpin,
{
    pub async fn send(&mut self, value: &T) -> Result<()> {
        self.buffer.resize(PREFIX_BYTES, 0);
        let bytes: u32 =
            bincode::encode_into_std_write(value, &mut self.buffer, bincode::config::standard())?
                .try_into()?;
        self.buffer[..PREFIX_BYTES].copy_from_slice(&bytes.to_be_bytes());

        self.writer.write_all(&self.buffer).await?;

        Ok(())
    }
}

#[derive(Debug)]
pub struct AsyncReceiver<T, R> {
    buffer: Vec<u8>,
    reader: R,
    _phantom: std::marker::PhantomData<T>,
}

impl<T, R> AsyncReceiver<T, R> {
    pub fn new(reader: R) -> Self {
        Self {
            buffer: Vec::with_capacity(BUFFER_SIZE),
            reader,
            _phantom: Default::default(),
        }
    }
}

impl<T, R> AsyncReceiver<T, R>
where
    T: Decode,
    R: AsyncRead + Unpin,
{
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

pub fn make_async_framed<T1: Encode, T2: Decode>(
    stream: tokio::net::TcpStream,
) -> (
    AsyncSender<T1, OwnedWriteHalf>,
    AsyncReceiver<T2, OwnedReadHalf>,
) {
    let (reader, writer) = stream.into_split();

    let sender = AsyncSender::new(writer);
    let receiver = AsyncReceiver::new(reader);

    (sender, receiver)
}
