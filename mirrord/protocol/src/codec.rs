use std::{
    collections::{HashMap, HashSet},
    io,
    marker::PhantomData,
    sync::LazyLock,
};

use actix_codec::{Decoder, Encoder};
use bincode::{
    BorrowDecode, Decode, Encode,
    enc::{
        EncoderImpl,
        write::{SizeWriter, Writer},
    },
    error::{DecodeError, EncodeError},
};
use bytes::{Buf, Bytes, BytesMut};
use derive_more::{Deref, From, Into};
use mirrord_macros::protocol_break;
use semver::VersionReq;

use crate::{
    ResponseError,
    dns::{
        GetAddrInfoRequest, GetAddrInfoRequestV2, GetAddrInfoResponse, ReverseDnsLookupRequest,
        ReverseDnsLookupResponse,
    },
    file::*,
    outgoing::{
        seqpacket::{DaemonSeqpacket, LayerSeqpacket},
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
    },
    tcp::{DaemonTcp, LayerTcp, LayerTcpSteal},
    vpn::{ClientVpn, ServerVpn},
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

#[derive(Encode, BorrowDecode, Debug, PartialEq, Eq, Clone, strum_macros::IntoStaticStr)]
#[bincode(decode_context = "crate::codec::DecodeCtx")]
#[strum(serialize_all = "lowercase")]
pub enum FileRequest {
    Open(OpenFileRequest),
    #[strum(serialize = "open")]
    OpenRelative(OpenRelativeFileRequest),
    Read(ReadFileRequest),
    #[strum(serialize = "read")]
    ReadLimited(ReadLimitedFileRequest),
    Seek(SeekFileRequest),
    Write(WriteFileRequest),
    #[strum(serialize = "write")]
    WriteLimited(WriteLimitedFileRequest),
    Close(CloseFileRequest),
    Access(AccessFileRequest),
    #[strum(serialize = "stat")]
    Xstat(XstatRequest),
    #[strum(serialize = "stat")]
    XstatFs(XstatFsRequest),
    #[strum(serialize = "opendir")]
    FdOpenDir(FdOpenDirRequest),
    #[strum(serialize = "readdir")]
    ReadDir(ReadDirRequest),
    #[strum(serialize = "closedir")]
    CloseDir(CloseDirRequest),
    #[strum(serialize = "getdents64")]
    GetDEnts64(GetDEnts64Request),
    #[strum(serialize = "readlink")]
    ReadLink(ReadLinkFileRequest),

    /// `readdir` request.
    ///
    /// Unlike other requests that come from the layer -> intproxy, this one is intproxy
    /// only. [`ReadDirRequest`]s that come from the layer are transformed into this
    /// batched form when the protocol version supports it. See [`READDIR_BATCH_VERSION`].
    #[strum(serialize = "readdir")]
    ReadDirBatch(ReadDirBatchRequest),
    #[strum(serialize = "mkdir")]
    MakeDir(MakeDirRequest),
    #[strum(serialize = "mkdir")]
    MakeDirAt(MakeDirAtRequest),
    #[strum(serialize = "rmdir")]
    RemoveDir(RemoveDirRequest),
    #[strum(serialize = "unlink")]
    Unlink(UnlinkRequest),
    #[strum(serialize = "unlink")]
    UnlinkAt(UnlinkAtRequest),
    #[strum(serialize = "statfs")]
    StatFs(StatFsRequest),

    /// Same as XstatFs, but results in the V2 response.
    #[strum(serialize = "stat")]
    XstatFsV2(XstatFsRequestV2),

    /// Same as StatFs, but results in the V2 response.
    #[strum(serialize = "statfs")]
    StatFsV2(StatFsRequestV2),
    Rename(RenameRequest),
    Ftruncate(FtruncateRequest),
    Futimens(FutimensRequest),
    Fchown(FchownRequest),
    Fchmod(FchmodRequest),
}

/// Minimal mirrord-protocol version that allows `ClientMessage::ReadyForLogs` message.
pub static CLIENT_READY_FOR_LOGS: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.3.1".parse().expect("Bad Identifier"));

/// `-layer` --> `-agent` messages.
#[derive(Encode, BorrowDecode, Debug, PartialEq, Eq, Clone)]
#[bincode(decode_context = "crate::codec::DecodeCtx")]
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
    /// Pong message that replies to [`DaemonMessage::OperatorPing`].
    ///
    /// Has the same ID that we got from the [`DaemonMessage::OperatorPing`].
    OperatorPong(u128),
    /// Reverse DNS lookup request (IP to hostname).
    ///
    /// Sent by the operator when enforcing hostname-based outgoing network policies.
    ReverseDnsLookup(ReverseDnsLookupRequest),

    /// Unix seqpacket outgoing message.
    ///
    /// These are the messages used by the `outgoing` feature (unix seqpacket), and handled by the
    /// `SeqpacketApi` in the agent.
    SeqpacketOutgoing(LayerSeqpacket),
}

/// Type alias for `Result`s that should be returned from mirrord-agent to mirrord-layer.
pub type RemoteResult<T> = Result<T, ResponseError>;

#[derive(Encode, BorrowDecode, Debug, PartialEq, Eq, Clone)]
#[bincode(decode_context = "crate::codec::DecodeCtx")]
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
    Rename(RemoteResult<()>),
    Ftruncate(RemoteResult<()>),
    Futimens(RemoteResult<()>),
    Fchown(RemoteResult<()>),
    Fchmod(RemoteResult<()>),
}

/// `-agent` --> `-layer` messages.
#[derive(Encode, BorrowDecode, PartialEq, Eq, Clone, Debug)]
#[bincode(decode_context = "crate::codec::DecodeCtx")]
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
    GetEnvVarsResponse(RemoteResult<RemoteEnvVars>),
    GetAddrInfoResponse(GetAddrInfoResponse),
    /// Pause is deprecated but we don't want to break protocol
    PauseTarget(crate::pause::DaemonPauseTarget),
    SwitchProtocolVersionResponse(#[bincode(with_serde)] semver::Version),
    Vpn(ServerVpn),
    /// Ping message that comes from the operator to mirrord.
    ///
    /// - Unlike other `DaemonMessage`s, this should never come from the agent!
    ///
    /// Holds the unique id of this ping.
    OperatorPing(u128),
    /// Reverse DNS lookup response.
    ///
    /// Sent by the agent in response to [`ClientMessage::ReverseDnsLookup`].
    ReverseDnsLookup(RemoteResult<ReverseDnsLookupResponse>),
    SeqpacketOutgoing(DaemonSeqpacket),
}

#[derive(Encode, Decode, PartialEq, Eq, Clone, From, Into, Deref)]
pub struct RemoteEnvVars(pub HashMap<String, String>);

impl core::fmt::Debug for RemoteEnvVars {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("GetEnvVarsResponse")
            .field(&"<REDACTED>")
            .finish()
    }
}

/// Opaque [`BorrowDecoder`](bincode::de::BorrowDecoder) context for decoding mirrord-protocol
/// messages.
///
/// Allows for decoding the data without actually moving the bytes in memory,
/// while still keeping the message types static. Note that this is only available
/// when decoding a message from a full raw bytes chunk (e.g. WebSocket binary message).
///
/// # How it works
///
/// Outside of this crate, this context is meant to be used only via [`Self::decode_from_bytes`]
/// (when you have the full message bytes) or [`ProtocolCodec`] (when you're processing a framed
/// stream).
///
/// Internally, this context keeps the full raw message [`Bytes`] (if available).
/// This [`Bytes`] instance is used by all [`Payload`](crate::Payload)s used in mirrord-protocol,
/// which borrow from it via cheap [`Bytes::slice_ref`].
pub struct DecodeCtx(
    /// This is only [`None`] when decoding from a data stream.
    Option<Bytes>,
);

impl DecodeCtx {
    /// Returns the full raw bytes of the message being decoded, if available.
    pub(crate) fn data(&self) -> Option<&Bytes> {
        self.0.as_ref()
    }

    /// Borrow-decodes a message from raw bytes, providing [`DecodeCtx`] context.
    ///
    /// If the message does not use the whole buffer, returns an error (leftover bytes).
    pub fn decode_from_bytes<M>(bytes: Bytes) -> Result<M, DecodeError>
    where
        M: for<'de> BorrowDecode<'de, Self>,
    {
        let context = Self(Some(bytes.clone()));
        bincode::borrow_decode_from_slice_with_context::<_, M, _>(
            &bytes,
            bincode::config::standard(),
            context,
        )
        .and_then(|output| {
            if output.1 != bytes.len() {
                Err(DecodeError::Other("detected leftover bytes"))
            } else {
                Ok(output.0)
            }
        })
    }

    /// Decodes a message from raw bytes received on a data stream, providing [`DecodeCtx`] context.
    ///
    /// This does not do borrow decoding.
    pub fn decode_from_data_stream<M>(buffer: &[u8]) -> Result<(M, usize), DecodeError>
    where
        M: for<'de> BorrowDecode<'de, Self>,
    {
        bincode::borrow_decode_from_slice_with_context::<_, M, _>(
            buffer,
            bincode::config::standard(),
            Self(None),
        )
    }
}

pub struct ProtocolCodec<I, O> {
    /// Phantom fields to make this struct generic over message types.
    _phantom_incoming_message: PhantomData<I>,
    _phantom_outgoing_message: PhantomData<O>,
}

impl<I, O> Copy for ProtocolCodec<I, O> {}

impl<I, O> Clone for ProtocolCodec<I, O> {
    fn clone(&self) -> Self {
        *self
    }
}

/// Codec to be used when receiving [`DaemonMessage`]s and sending [`ClientMessage`]s.
pub type ClientCodec = ProtocolCodec<DaemonMessage, ClientMessage>;

/// Codec to be used when receiving [`ClientMessage`]s and sending [`DaemonMessage`]s.
pub type DaemonCodec = ProtocolCodec<ClientMessage, DaemonMessage>;

impl<I, O> Default for ProtocolCodec<I, O> {
    fn default() -> Self {
        Self {
            _phantom_incoming_message: Default::default(),
            _phantom_outgoing_message: Default::default(),
        }
    }
}

impl<I, O> Decoder for ProtocolCodec<I, O>
where
    I: for<'de> bincode::BorrowDecode<'de, DecodeCtx>,
{
    type Item = I;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<Self::Item>> {
        // We don't know the length of the message, so we can't do borrow decoding here.
        match DecodeCtx::decode_from_data_stream::<I>(&src) {
            Ok((message, consumed)) => {
                src.advance(consumed);
                Ok(Some(message))
            }
            Err(DecodeError::UnexpectedEnd { .. }) => Ok(None),
            Err(error) => Err(io::Error::other(error)),
        }
    }
}

impl<I, O: bincode::Encode> Encoder<O> for ProtocolCodec<I, O> {
    type Error = io::Error;

    fn encode(&mut self, msg: O, dst: &mut BytesMut) -> Result<(), Self::Error> {
        // First, calculate the size of encoded message, and eagerly reserve enough space in the
        // buffer. This guarantees at most one allocation.
        let size = {
            let mut size_writer =
                EncoderImpl::new(SizeWriter::default(), bincode::config::standard());
            msg.encode(&mut size_writer).map_err(io::Error::other)?;
            size_writer.into_writer().bytes_written
        };
        dst.reserve(size);

        /// Allows using [`BytesMut`] as bincode's [`Writer`].
        struct WriterAdapter<'a>(&'a mut BytesMut);

        impl Writer for WriterAdapter<'_> {
            fn write(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
                self.0.extend_from_slice(bytes);
                Ok(())
            }
        }

        bincode::encode_into_writer(msg, WriterAdapter(dst), bincode::config::standard())
            .map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use bytes::{BufMut, BytesMut};

    use super::*;
    use crate::tcp::TcpData;

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
            bytes: [1, 2, 3].as_slice().into(),
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
