use core::fmt;
use std::{
    collections::{HashMap, HashSet},
    io::{self, SeekFrom},
    path::PathBuf,
};

use actix_codec::{Decoder, Encoder};
use bincode::{error::DecodeError, Decode, Encode};
use bytes::{Buf, BufMut, BytesMut};

use crate::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
    },
    tcp::{DaemonTcp, LayerTcp, LayerTcpSteal},
    ResponseError,
};

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct LogMessage {
    pub message: String,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct ReadFileRequest {
    pub fd: usize,
    pub buffer_size: usize,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct ReadLineFileRequest {
    pub fd: usize,
    pub buffer_size: usize,
}

// TODO: We're not handling `custom_flags` here, if we ever need to do so, add them here (it's an OS
// specific thing).
//
// TODO: Should probably live in a separate place (same reasoning as `AddrInfoHint`).
#[derive(Encode, Decode, Debug, PartialEq, Clone, Copy, Eq, Default)]
pub struct OpenOptionsInternal {
    pub read: bool,
    pub write: bool,
    pub append: bool,
    pub truncate: bool,
    pub create: bool,
    pub create_new: bool,
}

impl OpenOptionsInternal {
    pub fn is_read_only(&self) -> bool {
        self.read && !(self.write || self.append || self.truncate || self.create || self.create_new)
    }
}

impl From<OpenOptionsInternal> for std::fs::OpenOptions {
    fn from(internal: OpenOptionsInternal) -> Self {
        let OpenOptionsInternal {
            read,
            write,
            append,
            truncate,
            create,
            create_new,
        } = internal;

        std::fs::OpenOptions::new()
            .read(read)
            .write(write)
            .append(append)
            .truncate(truncate)
            .create(create)
            .create_new(create_new)
            .to_owned()
    }
}

/// Alternative to `std::io::SeekFrom`, used to implement `bincode::Encode` and `bincode::Decode`.
#[derive(Encode, Decode, Debug, PartialEq, Clone, Copy, Eq)]
pub enum SeekFromInternal {
    Start(u64),
    End(i64),
    Current(i64),
}

impl const From<SeekFromInternal> for SeekFrom {
    fn from(seek_from: SeekFromInternal) -> Self {
        match seek_from {
            SeekFromInternal::Start(start) => SeekFrom::Start(start),
            SeekFromInternal::End(end) => SeekFrom::End(end),
            SeekFromInternal::Current(current) => SeekFrom::Current(current),
        }
    }
}

impl const From<SeekFrom> for SeekFromInternal {
    fn from(seek_from: SeekFrom) -> Self {
        match seek_from {
            SeekFrom::Start(start) => SeekFromInternal::Start(start),
            SeekFrom::End(end) => SeekFromInternal::End(end),
            SeekFrom::Current(current) => SeekFromInternal::Current(current),
        }
    }
}
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct OpenFileRequest {
    pub path: PathBuf,
    pub open_options: OpenOptionsInternal,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct OpenRelativeFileRequest {
    pub relative_fd: usize,
    pub path: PathBuf,
    pub open_options: OpenOptionsInternal,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct SeekFileRequest {
    pub fd: usize,
    pub seek_from: SeekFromInternal,
}

#[derive(Encode, Decode, PartialEq, Eq, Clone)]
pub struct WriteFileRequest {
    pub fd: usize,
    pub write_bytes: Vec<u8>,
}

impl fmt::Debug for WriteFileRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WriteFileRequest")
            .field("fd", &self.fd)
            .field("write_bytes (length)", &self.write_bytes.len())
            .finish()
    }
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct CloseFileRequest {
    pub fd: usize,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct AccessFileRequest {
    pub pathname: PathBuf,
    pub mode: u8,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct GetEnvVarsRequest {
    pub env_vars_filter: HashSet<String>,
    pub env_vars_select: HashSet<String>,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum FileRequest {
    Open(OpenFileRequest),
    OpenRelative(OpenRelativeFileRequest),
    Read(ReadFileRequest),
    ReadLine(ReadLineFileRequest),
    Seek(SeekFileRequest),
    Write(WriteFileRequest),
    Close(CloseFileRequest),
    Access(AccessFileRequest),
}
/// `-layer` --> `-agent` messages.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum ClientMessage {
    Close,
    Tcp(LayerTcp),
    TcpSteal(LayerTcpSteal),
    TcpOutgoing(LayerTcpOutgoing),
    UdpOutgoing(LayerUdpOutgoing),
    FileRequest(FileRequest),
    GetEnvVarsRequest(GetEnvVarsRequest),
    Ping,
    GetAddrInfoRequest(GetAddrInfoRequest),
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct OpenFileResponse {
    pub fd: usize,
}

#[derive(Encode, Decode, PartialEq, Eq, Clone)]
pub struct ReadFileResponse {
    pub bytes: Vec<u8>,
    pub read_amount: usize,
}

#[derive(Encode, Decode, PartialEq, Eq, Clone)]
pub struct ReadLineFileResponse {
    pub bytes: Vec<u8>,
    pub read_amount: usize,
}

impl fmt::Debug for ReadFileResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadFileResponse")
            .field("bytes (length)", &self.bytes.len())
            .field("read_amount", &self.read_amount)
            .finish()
    }
}

impl fmt::Debug for ReadLineFileResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadLineFileResponse")
            .field("bytes (length)", &self.bytes.len())
            .field("read_amount", &self.read_amount)
            .finish()
    }
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct SeekFileResponse {
    pub result_offset: u64,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct WriteFileResponse {
    pub written_amount: usize,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct CloseFileResponse;

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct AccessFileResponse;

/// Type alias for `Result`s that should be returned from mirrord-agent to mirrord-layer.
pub type RemoteResult<T> = Result<T, ResponseError>;

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum FileResponse {
    Open(RemoteResult<OpenFileResponse>),
    Read(RemoteResult<ReadFileResponse>),
    ReadLine(RemoteResult<ReadLineFileResponse>),
    Seek(RemoteResult<SeekFileResponse>),
    Write(RemoteResult<WriteFileResponse>),
    Close(RemoteResult<CloseFileResponse>),
    Access(RemoteResult<AccessFileResponse>),
}
/// `-agent` --> `-layer` messages.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum DaemonMessage {
    Close,
    Tcp(DaemonTcp),
    TcpSteal(DaemonTcp),
    TcpOutgoing(DaemonTcpOutgoing),
    UdpOutgoing(DaemonUdpOutgoing),
    LogMessage(LogMessage),
    File(FileResponse),
    Pong,
    GetEnvVarsResponse(RemoteResult<HashMap<String, String>>),
    GetAddrInfoResponse(GetAddrInfoResponse),
}

pub struct ClientCodec {
    config: bincode::config::Configuration,
}

impl ClientCodec {
    pub fn new() -> Self {
        ClientCodec {
            config: bincode::config::standard(),
        }
    }
}

impl Default for ClientCodec {
    fn default() -> Self {
        ClientCodec::new()
    }
}

impl Decoder for ClientCodec {
    type Item = DaemonMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<Self::Item>> {
        match bincode::decode_from_slice(&src[..], self.config) {
            Ok((message, read)) => {
                src.advance(read);
                Ok(Some(message))
            }
            Err(DecodeError::UnexpectedEnd { .. }) => Ok(None),
            Err(err) => Err(io::Error::new(io::ErrorKind::Other, err.to_string())),
        }
    }
}

impl Encoder<ClientMessage> for ClientCodec {
    type Error = io::Error;

    fn encode(&mut self, msg: ClientMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let encoded = match bincode::encode_to_vec(msg, self.config) {
            Ok(encoded) => encoded,
            Err(err) => {
                return Err(io::Error::new(io::ErrorKind::Other, err.to_string()));
            }
        };
        dst.reserve(encoded.len());
        dst.put(&encoded[..]);

        Ok(())
    }
}

pub struct DaemonCodec {
    config: bincode::config::Configuration,
}

impl DaemonCodec {
    pub fn new() -> Self {
        DaemonCodec {
            config: bincode::config::standard(),
        }
    }
}

impl Default for DaemonCodec {
    fn default() -> Self {
        DaemonCodec::new()
    }
}

impl Decoder for DaemonCodec {
    type Item = ClientMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<Self::Item>> {
        match bincode::decode_from_slice(&src[..], self.config) {
            Ok((message, read)) => {
                src.advance(read);
                Ok(Some(message))
            }
            Err(DecodeError::UnexpectedEnd { .. }) => Ok(None),
            Err(err) => Err(io::Error::new(io::ErrorKind::Other, err.to_string())),
        }
    }
}

impl Encoder<DaemonMessage> for DaemonCodec {
    type Error = io::Error;

    fn encode(&mut self, msg: DaemonMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let encoded = match bincode::encode_to_vec(msg, self.config) {
            Ok(encoded) => encoded,
            Err(err) => {
                return Err(io::Error::new(io::ErrorKind::Other, err.to_string()));
            }
        };
        dst.reserve(encoded.len());
        dst.put(&encoded[..]);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bytes::BytesMut;

    use super::*;
    use crate::tcp::TcpData;

    #[test]
    fn sanity_client_encode_decode() {
        let mut client_codec = ClientCodec::new();
        let mut daemon_codec = DaemonCodec::new();
        let mut buf = BytesMut::new();

        let msg = ClientMessage::Tcp(LayerTcp::PortSubscribe(1));

        client_codec.encode(msg.clone(), &mut buf).unwrap();

        let decoded = daemon_codec.decode(&mut buf).unwrap().unwrap();

        assert_eq!(decoded, msg);
        assert!(buf.is_empty());
    }

    #[test]
    fn sanity_daemon_encode_decode() {
        let mut client_codec = ClientCodec::new();
        let mut daemon_codec = DaemonCodec::new();
        let mut buf = BytesMut::new();

        let msg = DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
            connection_id: 1,
            bytes: vec![1, 2, 3],
        }));

        daemon_codec.encode(msg.clone(), &mut buf).unwrap();

        let decoded = client_codec.decode(&mut buf).unwrap().unwrap();

        assert_eq!(decoded, msg);
        assert!(buf.is_empty());
    }

    #[test]
    fn decode_client_invalid_data() {
        let mut codec = ClientCodec::new();
        let mut buf = BytesMut::new();
        buf.put_u8(254);

        let res = codec.decode(&mut buf);
        match res {
            Ok(_) => panic!("Should have failed"),
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::Other),
        }
    }

    #[test]
    fn decode_client_partial_data() {
        let mut codec = ClientCodec::new();
        let mut buf = BytesMut::new();
        buf.put_u8(1);

        assert!(codec.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn decode_daemon_invalid_data() {
        let mut codec = DaemonCodec::new();
        let mut buf = BytesMut::new();
        buf.put_u8(254);

        let res = codec.decode(&mut buf);
        match res {
            Ok(_) => panic!("Should have failed"),
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::Other),
        }
    }
}
