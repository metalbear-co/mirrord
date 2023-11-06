//! Custom codec used in `layer <-> proxy` communication.
//! Supports both synchronous (required by the layer) and asynchronous (convenient for the proxy)
//! IO. Asynchronous IO is feature-gated with the `codec-async` feature.
//!
//! An encoded message consists of two parts:
//! * prefix: 4 bytes containing payload length in bytes (big-endian [`u32`])
//! * payload: a value of a known type, encoded with [`bincode`]

use std::{
    io::{self, ErrorKind, Read, Write},
    num::TryFromIntError,
};

use bincode::{
    error::{DecodeError, EncodeError},
    Decode, Encode,
};
use thiserror::Error;

#[cfg(feature = "codec-async")]
mod codec_async;
#[cfg(feature = "codec-async")]
pub use codec_async::*;

/// Errors that can occur when using this codec.
#[derive(Error, Debug)]
pub enum CodecError {
    /// Encoding a message failed.
    #[error("encoding failed: {0}")]
    EncodeError(#[from] EncodeError),
    /// Decoding a message failed.
    #[error("decoding failed: {0}")]
    DecodeError(#[from] DecodeError),
    /// Encoded message was too long for this codec.
    #[error("message too long: {0}")]
    MessageTooLongError(#[from] TryFromIntError),
    /// IO failed.
    #[error("io failed: {0}")]
    IoError(#[from] io::Error),
}

/// Alias for [`Result`](core::result::Result) type used by this codec.
pub type Result<T> = core::result::Result<T, CodecError>;

/// Initial buffer size used by wrappers in this module.
/// Buffers are used to encode and decode messages in memory.
const BUFFER_SIZE: usize = 1024;

/// Length of the message prefix used by this codec.
/// Determines the maximum length of the message.
const PREFIX_BYTES: usize = u32::BITS as usize / 8;

/// Handles sending messages of type `T` through the underlying [Write] of type `W`.
#[derive(Debug)]
pub struct SyncEncoder<T, W> {
    buffer: Vec<u8>,
    writer: W,
    _phantom: std::marker::PhantomData<fn() -> T>,
}

impl<T, W> SyncEncoder<T, W> {
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

impl<T, W> SyncEncoder<T, W>
where
    T: Encode,
    W: Write,
{
    /// Encodes the given value into the inner IO handler.
    pub fn send(&mut self, value: &T) -> Result<()> {
        self.buffer.resize(PREFIX_BYTES, 0);
        let bytes: u32 =
            bincode::encode_into_std_write(value, &mut self.buffer, bincode::config::standard())?
                .try_into()?;
        self.buffer
            .get_mut(..PREFIX_BYTES)
            .expect("buffer too short")
            .copy_from_slice(&bytes.to_be_bytes());

        self.writer.write_all(&self.buffer)?;

        Ok(())
    }

    /// Flushes the inner IO handler.
    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush().map_err(Into::into)
    }
}

/// Handles receiving messages of type `T` from the underlying [Write] of type `W`.
#[derive(Debug)]
pub struct SyncDecoder<T, R> {
    buffer: Vec<u8>,
    reader: R,
    _phantom: std::marker::PhantomData<fn() -> T>,
}

impl<T, R> SyncDecoder<T, R> {
    /// Wraps the unerlying IO handler.
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

impl<T, R> SyncDecoder<T, R>
where
    T: Decode,
    R: Read,
{
    /// Decodes the next message from the underlying IO handler.
    /// Does not read any excessive bytes.
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

/// Creates a new pair of [`SyncEncoder`] and [`SyncDecoder`], using the given synchronous
/// [`TcpStream`](std::net::TcpStream).
pub fn make_sync_framed<T1: Encode, T2: Decode>(
    stream: std::net::TcpStream,
) -> Result<(
    SyncEncoder<T1, std::net::TcpStream>,
    SyncDecoder<T2, std::net::TcpStream>,
)> {
    let stream_cloned = stream.try_clone()?;

    let sender = SyncEncoder::new(stream);
    let receiver = SyncDecoder::new(stream_cloned);

    Ok((sender, receiver))
}
