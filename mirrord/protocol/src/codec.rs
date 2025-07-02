use std::{
    collections::{HashMap, HashSet},
    io,
    marker::PhantomData,
    sync::LazyLock,
};

use actix_codec::{Decoder, Encoder};
use bincode::{error::DecodeError, Decode, Encode};
use bytes::{Buf, BufMut, BytesMut};
use mirrord_macros::protocol_break;
use semver::VersionReq;

use crate::{
    dns::{GetAddrInfoRequest, GetAddrInfoRequestV2, GetAddrInfoResponse},
    file::*,
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
    },
    tcp::{DaemonTcp, LayerTcp, LayerTcpSteal},
    vpn::{ClientVpn, ServerVpn},
    ResponseError,
};

/// Minimal mirrord-protocol version that that allows [`LogLevel::Info`].
pub static INFO_LOG_VERSION: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.13.4".parse().expect("Bad Identifier"));

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone, Copy)]
pub enum LogLevel {
    Warn,
    Error,
    /// Supported from [`INFO_LOG_VERSION`].
    Info,
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
    ReadLink(ReadLinkFileRequest),

    /// `readdir` request.
    ///
    /// Unlike other requests that come from the layer -> intproxy, this one is intproxy
    /// only. [`ReadDirRequest`]s that come from the layer are transformed into this
    /// batched form when the protocol version supports it. See [`READDIR_BATCH_VERSION`].
    ReadDirBatch(ReadDirBatchRequest),
    MakeDir(MakeDirRequest),
    MakeDirAt(MakeDirAtRequest),
    RemoveDir(RemoveDirRequest),
    Unlink(UnlinkRequest),
    UnlinkAt(UnlinkAtRequest),
    StatFs(StatFsRequest),

    /// Same as XstatFs, but results in the V2 response.
    XstatFsV2(XstatFsRequestV2),

    /// Same as StatFs, but results in the V2 response.
    StatFsV2(StatFsRequestV2),
}

/// Minimal mirrord-protocol version that allows `ClientMessage::ReadyForLogs` message.
pub static CLIENT_READY_FOR_LOGS: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.3.1".parse().expect("Bad Identifier"));

/// `-layer` --> `-agent` messages.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum ClientMessage {
    Close,
    /// TCP sniffer message.
    ///
    /// These are the messages used by the `mirror` feature, and handled by the
    /// `TcpSnifferApi` in the agent.
    Tcp(LayerTcp),

    /// TCP stealer message.
    ///
    /// These are the messages used by the `steal` feature, and handled by the `TcpStealerApi` in
    /// the agent.
    TcpSteal(LayerTcpSteal),
    /// TCP outgoing message.
    ///
    /// These are the messages used by the `outgoing` feature (tcp), and handled by the
    /// `TcpOutgoingApi` in the agent.
    TcpOutgoing(LayerTcpOutgoing),

    /// UDP outgoing message.
    ///
    /// These are the messages used by the `outgoing` feature (udp), and handled by the
    /// `UdpOutgoingApi` in the agent.
    UdpOutgoing(LayerUdpOutgoing),
    FileRequest(FileRequest),
    GetEnvVarsRequest(GetEnvVarsRequest),
    Ping,
    GetAddrInfoRequest(GetAddrInfoRequest),
    /// Whether to pause or unpause the target container.
    PauseTargetRequest(bool),
    SwitchProtocolVersion(#[bincode(with_serde)] semver::Version),
    ReadyForLogs,
    Vpn(ClientVpn),
    GetAddrInfoRequestV2(GetAddrInfoRequestV2),
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
    ReadLink(RemoteResult<ReadLinkFileResponse>),
    ReadDirBatch(RemoteResult<ReadDirBatchResponse>),
    MakeDir(RemoteResult<()>),
    RemoveDir(RemoteResult<()>),
    Unlink(RemoteResult<()>),
    XstatFsV2(RemoteResult<XstatFsResponseV2>),
}

/// `-agent` --> `-layer` messages.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
#[protocol_break(2)]
#[allow(deprecated)] // We can't remove deprecated variants without breaking the protocol
pub enum DaemonMessage {
    /// Kills the intproxy, no guarantee that messages that were sent before a `Close` will be
    /// handled by the intproxy and forwarded to the layer before the intproxy exits.
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
    /// Pause is deprecated but we don't want to break protocol
    PauseTarget(crate::pause::DaemonPauseTarget),
    SwitchProtocolVersionResponse(#[bincode(with_serde)] semver::Version),
    Vpn(ServerVpn),
}

pub struct ProtocolCodec<I, O> {
    config: bincode::config::Configuration,
    /// Phantom fields to make this struct generic over message types.
    _phantom_incoming_message: PhantomData<I>,
    _phantom_outgoing_message: PhantomData<O>,
}

// Codec to be used by the client side to receive `DaemonMessage`s from the agent and send
// `ClientMessage`s to the agent.
pub type ClientCodec = ProtocolCodec<DaemonMessage, ClientMessage>;
// Codec to be used by the agent side to receive `ClientMessage`s from the client and send
// `DaemonMessage`s to the client.
pub type DaemonCodec = ProtocolCodec<ClientMessage, DaemonMessage>;

impl<I, O> Default for ProtocolCodec<I, O> {
    fn default() -> Self {
        Self {
            config: bincode::config::standard(),
            _phantom_incoming_message: Default::default(),
            _phantom_outgoing_message: Default::default(),
        }
    }
}

impl<I: bincode::Decode<()>, O> Decoder for ProtocolCodec<I, O> {
    type Item = I;
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

impl<I, O: bincode::Encode> Encoder<O> for ProtocolCodec<I, O> {
    type Error = io::Error;

    fn encode(&mut self, msg: O, dst: &mut BytesMut) -> Result<(), Self::Error> {
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
    use crate::{tcp::TcpData, Payload};

    #[test]
    fn sanity_client_encode_decode() {
        let mut client_codec = ClientCodec::default();
        let mut daemon_codec = DaemonCodec::default();
        let mut buf = BytesMut::new();

        let msg = ClientMessage::Tcp(LayerTcp::PortSubscribe(1));

        client_codec.encode(msg.clone(), &mut buf).unwrap();

        let decoded = daemon_codec.decode(&mut buf).unwrap().unwrap();

        assert_eq!(decoded, msg);
        assert!(buf.is_empty());
    }

    #[test]
    fn sanity_daemon_encode_decode() {
        let mut client_codec = ClientCodec::default();
        let mut daemon_codec = DaemonCodec::default();
        let mut buf = BytesMut::new();

        let msg = DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
            connection_id: 1,
            bytes: Payload::from(vec![1, 2, 3]),
        }));

        daemon_codec.encode(msg.clone(), &mut buf).unwrap();

        let decoded = client_codec.decode(&mut buf).unwrap().unwrap();

        assert_eq!(decoded, msg);
        assert!(buf.is_empty());
    }

    #[test]
    fn decode_client_invalid_data() {
        let mut codec = ClientCodec::default();
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
        let mut codec = ClientCodec::default();
        let mut buf = BytesMut::new();
        buf.put_u8(1);

        assert!(codec.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn decode_daemon_invalid_data() {
        let mut codec = DaemonCodec::default();
        let mut buf = BytesMut::new();
        buf.put_u8(254);

        let res = codec.decode(&mut buf);
        match res {
            Ok(_) => panic!("Should have failed"),
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::Other),
        }
    }
}
