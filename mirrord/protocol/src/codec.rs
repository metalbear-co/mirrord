use std::{
    collections::{HashMap, HashSet},
    fmt, io,
    marker::PhantomData,
    ops::Deref,
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
    share_link::ShareLinkRequest,
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

    /// Registers or removes a session key the agent accepts from share links.
    ///
    /// Sent by the operator. Supported from
    /// [`SHARE_LINK_VERSION`](crate::share_link::SHARE_LINK_VERSION).
    ShareLink(ShareLinkRequest),
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

/// The feature a [`ClientMessage`] or [`DaemonMessage`] belongs to, for reporting how much memory
/// messages take.
///
/// Only the features that can move a lot of data get their own kind, and every other message is
/// [`MessageKind::Other`], so the kinds of one direction add up to its total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum_macros::EnumIter)]
pub enum MessageKind {
    Tcp,
    TcpSteal,
    TcpOutgoing,
    UdpOutgoing,
    SeqpacketOutgoing,
    File,
    Other,
}

impl MessageKind {
    /// A `snake_case` name for the kind, used as a metric label value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::TcpSteal => "tcp_steal",
            Self::TcpOutgoing => "tcp_outgoing",
            Self::UdpOutgoing => "udp_outgoing",
            Self::SeqpacketOutgoing => "seqpacket_outgoing",
            Self::File => "file",
            Self::Other => "other",
        }
    }
}

impl ClientMessage {
    /// The [`MessageKind`] this message is reported under.
    pub fn kind(&self) -> MessageKind {
        match self {
            Self::Tcp(..) => MessageKind::Tcp,
            Self::TcpSteal(..) => MessageKind::TcpSteal,
            Self::TcpOutgoing(..) => MessageKind::TcpOutgoing,
            Self::UdpOutgoing(..) => MessageKind::UdpOutgoing,
            Self::SeqpacketOutgoing(..) => MessageKind::SeqpacketOutgoing,
            Self::FileRequest(..) => MessageKind::File,
            _ => MessageKind::Other,
        }
    }
}

impl DaemonMessage {
    /// The [`MessageKind`] this message is reported under.
    pub fn kind(&self) -> MessageKind {
        match self {
            Self::Tcp(..) => MessageKind::Tcp,
            Self::TcpSteal(..) => MessageKind::TcpSteal,
            Self::TcpOutgoing(..) => MessageKind::TcpOutgoing,
            Self::UdpOutgoing(..) => MessageKind::UdpOutgoing,
            Self::SeqpacketOutgoing(..) => MessageKind::SeqpacketOutgoing,
            Self::File(..) => MessageKind::File,
            _ => MessageKind::Other,
        }
    }
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
        Ok(decode_frame(src)?.map(|(message, _)| message))
    }
}

impl<I, O: bincode::Encode> Encoder<O> for ProtocolCodec<I, O> {
    type Error = io::Error;

    fn encode(&mut self, msg: O, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let size = encoded_size(&msg).map_err(io::Error::other)?;
        encode_reserved(msg, size, dst)
    }
}

/// A mirrord-protocol message together with its bincode-encoded size.
///
/// Lets components account for the memory that messages hold (for metrics) without encoding
/// them twice: the size comes from the decoder, or is computed once in [`Measured::new`], and
/// [`MeasuredCodec`] reuses it to reserve the output buffer. It derefs to the message, but gives
/// no mutable access to it, so the size cannot go stale.
pub struct Measured<M> {
    message: M,
    encoded_len: usize,
}

impl<M: Encode> Measured<M> {
    /// Measures `message` with a bincode encoding pass that only counts the bytes.
    ///
    /// If the message cannot be encoded, the size is 0. Encoding it later fails with the same
    /// error, so a wrong size only affects the memory accounting and the buffer reservation.
    pub fn new(message: M) -> Self {
        let encoded_len = encoded_size(&message).unwrap_or(0);
        Self {
            message,
            encoded_len,
        }
    }
}

impl<M> Measured<M> {
    /// The number of bytes this message takes when bincode-encoded.
    pub fn encoded_len(&self) -> usize {
        self.encoded_len
    }

    /// Returns the message, dropping its size.
    pub fn into_inner(self) -> M {
        self.message
    }
}

impl<M> Deref for Measured<M> {
    type Target = M;

    fn deref(&self) -> &Self::Target {
        &self.message
    }
}

impl<M: fmt::Debug> fmt::Debug for Measured<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

/// [`ProtocolCodec`] that decodes [`Measured`] messages and encodes them reusing their size.
///
/// The decoded size is the number of bytes the frame took in the stream. The wire format is the
/// same as [`ProtocolCodec`]'s, so either side of a connection can use either codec.
pub struct MeasuredCodec<I, O> {
    /// Phantom fields to make this struct generic over message types.
    _phantom_incoming_message: PhantomData<I>,
    _phantom_outgoing_message: PhantomData<O>,
}

/// [`MeasuredCodec`] to be used when receiving [`DaemonMessage`]s and sending [`ClientMessage`]s.
pub type MeasuredClientCodec = MeasuredCodec<DaemonMessage, ClientMessage>;

/// [`MeasuredCodec`] to be used when receiving [`ClientMessage`]s and sending [`DaemonMessage`]s.
pub type MeasuredDaemonCodec = MeasuredCodec<ClientMessage, DaemonMessage>;

impl<I, O> Default for MeasuredCodec<I, O> {
    fn default() -> Self {
        Self {
            _phantom_incoming_message: Default::default(),
            _phantom_outgoing_message: Default::default(),
        }
    }
}

impl<I, O> Decoder for MeasuredCodec<I, O>
where
    I: for<'de> bincode::BorrowDecode<'de, DecodeCtx>,
{
    type Item = Measured<I>;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<Self::Item>> {
        Ok(decode_frame(src)?.map(|(message, encoded_len)| Measured {
            message,
            encoded_len,
        }))
    }
}

impl<I, O: bincode::Encode> Encoder<Measured<O>> for MeasuredCodec<I, O> {
    type Error = io::Error;

    fn encode(&mut self, msg: Measured<O>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        encode_reserved(msg.message, msg.encoded_len, dst)
    }
}

/// Decodes one message from the start of `src` and removes its bytes from `src`.
///
/// Returns the message with the number of bytes it took, or [`None`] when `src` does not hold a
/// whole message yet.
fn decode_frame<M>(src: &mut BytesMut) -> io::Result<Option<(M, usize)>>
where
    M: for<'de> bincode::BorrowDecode<'de, DecodeCtx>,
{
    // We don't know the length of the message, so we can't do borrow decoding here.
    match DecodeCtx::decode_from_data_stream::<M>(src) {
        Ok((message, consumed)) => {
            src.advance(consumed);
            Ok(Some((message, consumed)))
        }
        Err(DecodeError::UnexpectedEnd { .. }) => Ok(None),
        Err(error) => Err(io::Error::other(error)),
    }
}

/// Computes the bincode-encoded size of `message` without allocating the encoded bytes.
fn encoded_size<M: Encode>(message: &M) -> Result<usize, EncodeError> {
    let mut size_writer = EncoderImpl::new(SizeWriter::default(), bincode::config::standard());
    message.encode(&mut size_writer)?;
    Ok(size_writer.into_writer().bytes_written)
}

/// Encodes `message` into `dst`, reserving `size` bytes first.
///
/// With the exact encoded size, the reservation guarantees at most one allocation. A wrong `size`
/// only costs extra allocations; the encoded bytes are the same.
fn encode_reserved<M: Encode>(message: M, size: usize, dst: &mut BytesMut) -> io::Result<()> {
    dst.reserve(size);

    /// Allows using [`BytesMut`] as bincode's [`Writer`].
    struct WriterAdapter<'a>(&'a mut BytesMut);

    impl Writer for WriterAdapter<'_> {
        fn write(&mut self, bytes: &[u8]) -> Result<(), EncodeError> {
            self.0.extend_from_slice(bytes);
            Ok(())
        }
    }

    bincode::encode_into_writer(message, WriterAdapter(dst), bincode::config::standard())
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use bytes::{BufMut, BytesMut};
    use hyper::{HeaderMap, Method, Version, header};

    use super::*;
    use crate::{
        outgoing::LayerClose,
        tcp::{ChunkedRequest, HttpRequest, InternalHttpBodyFrame, InternalHttpRequest, TcpData},
    };

    /// Messages whose encoding includes the parts of the format that vary in length: payloads,
    /// variable-length integers, and HTTP heads with headers.
    fn measured_test_messages() -> (Vec<ClientMessage>, Vec<DaemonMessage>) {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "example.com".parse().unwrap());
        headers.insert(header::TRANSFER_ENCODING, "chunked".parse().unwrap());

        let client_messages = vec![
            ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
                connection_id: u64::MAX,
                bytes: vec![7; 1024].into(),
            })),
            ClientMessage::FileRequest(FileRequest::Read(ReadFileRequest {
                remote_fd: 3,
                buffer_size: 4096,
            })),
        ];
        let daemon_messages = vec![
            DaemonMessage::File(FileResponse::Read(Ok(ReadFileResponse {
                bytes: b"file contents".to_vec().into(),
                read_amount: 13,
            }))),
            DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(ChunkedRequest::StartV1(
                HttpRequest {
                    internal_request: InternalHttpRequest {
                        method: Method::POST,
                        uri: "/upload?id=1".parse().unwrap(),
                        headers,
                        version: Version::HTTP_11,
                        body: vec![InternalHttpBodyFrame::Data(b"first chunk".to_vec().into())],
                    },
                    connection_id: 1,
                    request_id: 2,
                    port: 80,
                },
            ))),
        ];

        (client_messages, daemon_messages)
    }

    /// Encodes `messages` back to back with [`ProtocolCodec`], then checks that [`MeasuredCodec`]
    /// decodes each with its own frame's length as the size, not the length of whatever else is
    /// buffered behind it.
    fn assert_decoded_sizes_are_frame_lengths<M>(messages: Vec<M>)
    where
        M: Encode + Clone + PartialEq + fmt::Debug + for<'de> BorrowDecode<'de, DecodeCtx>,
    {
        let mut buf = BytesMut::new();
        let mut frame_lens = Vec::new();
        for message in &messages {
            let before = buf.len();
            ProtocolCodec::<(), M>::default()
                .encode(message.clone(), &mut buf)
                .unwrap();
            frame_lens.push(buf.len() - before);
        }

        let mut codec = MeasuredCodec::<M, ()>::default();
        for (message, frame_len) in messages.into_iter().zip(frame_lens) {
            let decoded = codec.decode(&mut buf).unwrap().unwrap();
            assert_eq!(decoded.encoded_len(), frame_len, "{message:?}");
            assert_eq!(
                decoded.encoded_len(),
                Measured::new(message.clone()).encoded_len()
            );
            assert_eq!(decoded.into_inner(), message);
        }
        assert!(buf.is_empty());
    }

    #[test]
    fn measured_codec_decoded_size_is_frame_length() {
        let (client_messages, daemon_messages) = measured_test_messages();

        assert_decoded_sizes_are_frame_lengths(client_messages);
        assert_decoded_sizes_are_frame_lengths(daemon_messages);
    }

    /// Consumers report each kind as its own series, so each feature's messages must map to its
    /// kind.
    #[test]
    fn message_kind_mapping() {
        let tcp_data = TcpData {
            connection_id: 1,
            bytes: Default::default(),
        };

        let client_messages = [
            (
                ClientMessage::Tcp(LayerTcp::PortSubscribe(80)),
                MessageKind::Tcp,
            ),
            (
                ClientMessage::TcpSteal(LayerTcpSteal::Data(tcp_data.clone())),
                MessageKind::TcpSteal,
            ),
            (
                ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(LayerClose {
                    connection_id: 1,
                })),
                MessageKind::TcpOutgoing,
            ),
            (
                ClientMessage::UdpOutgoing(LayerUdpOutgoing::Close(LayerClose {
                    connection_id: 1,
                })),
                MessageKind::UdpOutgoing,
            ),
            (
                ClientMessage::SeqpacketOutgoing(LayerSeqpacket::Close(LayerClose {
                    connection_id: 1,
                })),
                MessageKind::SeqpacketOutgoing,
            ),
            (
                ClientMessage::FileRequest(FileRequest::Close(CloseFileRequest { fd: 3 })),
                MessageKind::File,
            ),
            (ClientMessage::Ping, MessageKind::Other),
        ];
        for (message, kind) in client_messages {
            assert_eq!(message.kind(), kind, "{message:?}");
        }

        let daemon_messages = [
            (
                DaemonMessage::Tcp(DaemonTcp::Data(tcp_data.clone())),
                MessageKind::Tcp,
            ),
            (
                DaemonMessage::TcpSteal(DaemonTcp::Data(tcp_data)),
                MessageKind::TcpSteal,
            ),
            (
                DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Close(1)),
                MessageKind::TcpOutgoing,
            ),
            (
                DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Close(1)),
                MessageKind::UdpOutgoing,
            ),
            (
                DaemonMessage::SeqpacketOutgoing(DaemonSeqpacket::Close(1)),
                MessageKind::SeqpacketOutgoing,
            ),
            (
                DaemonMessage::File(FileResponse::Read(Ok(ReadFileResponse {
                    bytes: Default::default(),
                    read_amount: 0,
                }))),
                MessageKind::File,
            ),
            (DaemonMessage::Pong, MessageKind::Other),
        ];
        for (message, kind) in daemon_messages {
            assert_eq!(message.kind(), kind, "{message:?}");
        }
    }

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
