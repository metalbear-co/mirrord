use std::{
    collections::{HashMap, HashSet},
    io,
    sync::LazyLock,
};

use actix_codec::{Decoder, Encoder};
use bincode::{error::DecodeError, Decode, Encode};
use bytes::{Buf, BufMut, BytesMut};
use mirrord_macros::protocol_break;
use semver::VersionReq;

use crate::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    file::{
        AccessFileRequest, AccessFileResponse, CloseDirRequest, CloseFileRequest, FdOpenDirRequest,
        GetDEnts64Request, GetDEnts64Response, OpenDirResponse, OpenFileRequest, OpenFileResponse,
        OpenRelativeFileRequest, ReadDirRequest, ReadDirResponse, ReadFileRequest,
        ReadFileResponse, ReadLimitedFileRequest, SeekFileRequest, SeekFileResponse,
        WriteFileRequest, WriteFileResponse, WriteLimitedFileRequest, XstatFsRequest,
        XstatFsResponse, XstatRequest, XstatResponse,
    },
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
    },
    pause::DaemonPauseTarget,
    tcp::{DaemonTcp, LayerTcp, LayerTcpSteal},
    ResponseError,
};

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone, Copy)]
pub enum LogLevel {
    Warn,
    Error,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct LogMessage {
    pub message: String,
    pub level: LogLevel,
}

impl LogMessage {
    pub fn warn(message: String) -> Self {
        Self {
            message,
            level: LogLevel::Warn,
        }
    }

    pub fn error(message: String) -> Self {
        Self {
            message,
            level: LogLevel::Error,
        }
    }
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
    ReadLimited(ReadLimitedFileRequest),
    Seek(SeekFileRequest),
    Write(WriteFileRequest),
    WriteLimited(WriteLimitedFileRequest),
    Close(CloseFileRequest),
    Access(AccessFileRequest),
    Xstat(XstatRequest),
    XstatFs(XstatFsRequest),
    FdOpenDir(FdOpenDirRequest),
    ReadDir(ReadDirRequest),
    CloseDir(CloseDirRequest),
    GetDEnts64(GetDEnts64Request),
}

pub static CLIENT_READY_FOR_LOGS: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.3.1".parse().expect("Bad Identifier"));

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
    /// Whether to pause or unpause the target container.
    PauseTargetRequest(bool),
    SwitchProtocolVersion(#[bincode(with_serde)] semver::Version),
    ReadyForLogs,
}

/// Type alias for `Result`s that should be returned from mirrord-agent to mirrord-layer.
pub type RemoteResult<T> = Result<T, ResponseError>;

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum FileResponse {
    Open(RemoteResult<OpenFileResponse>),
    Read(RemoteResult<ReadFileResponse>),
    ReadLimited(RemoteResult<ReadFileResponse>),
    Write(RemoteResult<WriteFileResponse>),
    WriteLimited(RemoteResult<WriteFileResponse>),
    Seek(RemoteResult<SeekFileResponse>),
    Access(RemoteResult<AccessFileResponse>),
    Xstat(RemoteResult<XstatResponse>),
    XstatFs(RemoteResult<XstatFsResponse>),
    ReadDir(RemoteResult<ReadDirResponse>),
    OpenDir(RemoteResult<OpenDirResponse>),
    GetDEnts64(RemoteResult<GetDEnts64Response>),
}

/// `-agent` --> `-layer` messages.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
#[protocol_break(2)]
pub enum DaemonMessage {
    Close(String),
    Tcp(DaemonTcp),
    TcpSteal(DaemonTcp),
    TcpOutgoing(DaemonTcpOutgoing),
    UdpOutgoing(DaemonUdpOutgoing),
    LogMessage(LogMessage),
    File(FileResponse),
    Pong,
    /// NOTE: can remove `RemoteResult` when we break protocol compatibility.
    GetEnvVarsResponse(RemoteResult<HashMap<String, String>>),
    GetAddrInfoResponse(GetAddrInfoResponse),
    PauseTarget(DaemonPauseTarget),
    SwitchProtocolVersionResponse(#[bincode(with_serde)] semver::Version),
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
