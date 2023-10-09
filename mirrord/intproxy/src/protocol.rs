//! Protocol used in communication between the layer and the internal proxy.
//! This protocol does not have to be backwards compatible and can be changed freely, as the
//! internal proxy and the layer are shipped together in a single binary.

use std::{fmt, net::SocketAddr};

use bincode::{Decode, Encode};
use mirrord_protocol::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    file::{
        AccessFileRequest, AccessFileResponse, CloseDirRequest, CloseFileRequest, FdOpenDirRequest,
        GetDEnts64Request, GetDEnts64Response, OpenDirResponse, OpenFileRequest, OpenFileResponse,
        OpenRelativeFileRequest, ReadDirRequest, ReadDirResponse, ReadFileRequest,
        ReadFileResponse, ReadLimitedFileRequest, SeekFileRequest, SeekFileResponse,
        WriteFileRequest, WriteFileResponse, WriteLimitedFileRequest, XstatFsRequest,
        XstatFsResponse, XstatRequest, XstatResponse,
    },
    outgoing::SocketAddress,
    FileRequest, FileResponse, LogMessage, Port, RemoteResult,
};

use crate::{bind_nested, impl_request};

mod macros;

/// An identifier for a message sent from the layer to the internal proxy.
/// The layer uses this to match proxy responses with awaiting requests.
pub type MessageId = u64;

/// Special [`MessageId`] used by the internal proxy to send messages that are not responses.
/// The layer should not use this identifier.
pub const NOT_A_RESPONSE: MessageId = MessageId::MAX;

/// A wrapper for messages sent through the `layer <-> proxy` connection.
#[derive(Encode, Decode, Debug)]
pub struct LocalMessage<T> {
    /// Message identifier.
    pub message_id: MessageId,
    /// The actual message.
    pub inner: T,
}

/// Messages sent by the layer and handled by the internal proxy.
#[derive(Encode, Decode, Debug)]
pub enum LayerToProxyMessage {
    /// A request to start new `layer <-> proxy` session.
    /// This should be the first message sent by the layer after opening a new connection to the
    /// internal proxy.
    NewSession(NewSessionRequest),
    /// A file operation request.
    File(FileRequest),
    /// A DNS request.
    GetAddrInfo(GetAddrInfoRequest),
    /// A request to initiate a new outgoing connection.
    OutgoingConnect(OutgoingConnectRequest),
    /// Requests related to incoming connections.
    Incoming(IncomingRequest),
}

/// Unique `layer <-> proxy` session identifier.
/// Each connection between the layer and the internal proxy belongs to a separate session.
/// New connection is established when the layer initializes or forks.
pub type SessionId = u64;

/// A layer's request to start a new session with the internal proxy.
/// Contains info about layer's state.
/// This should be the first message sent by the layer after opening a new connection to the
/// internal proxy.
///
/// # Note
///
/// Sharing state between [`exec`](https://man7.org/linux/man-pages/man3/exec.3.html) calls is currently not supported.
/// Therefore, when the layer initializes, it uses [`NewSessionRequest::New`] and does not inherit
/// any state.
#[derive(Encode, Decode, Debug)]
pub enum NewSessionRequest {
    /// Layer initialized from its constructor and has a fresh state.
    New,
    /// Layer re-initialized from a [`fork`](https://man7.org/linux/man-pages/man2/fork.2.html) detour.
    /// It inherits state from its parent.
    Forked(SessionId),
}

/// Supported network protocols when intercepting outgoing connections.
#[derive(Encode, Decode, Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum NetProtocol {
    /// Data stream over IP (TCP) or UDS.
    Stream,
    /// Datagrams over IP (UDP). UDS is not supported.
    ///
    /// # Note
    ///
    /// In reality, this is a connectionless protocol.
    /// However, one can call [`connect`](https://man7.org/linux/man-pages/man2/connect.2.html) on a datagram socket,
    /// which alters this socket's behavior. Currently, we require this call to happen before we
    /// intercept outgoing UDP.
    Datagrams,
}

impl fmt::Display for NetProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::Stream => "STREAM",
            Self::Datagrams => "DGRAM",
        };

        f.write_str(as_str)
    }
}

/// A request to initiate a new outgoing connection.
#[derive(Encode, Decode, Debug)]
pub struct OutgoingConnectRequest {
    /// The address the user application tries to connect to.
    pub remote_address: SocketAddress,
    /// The protocol stack the user application wants to use.
    pub protocol: NetProtocol,
}

/// Requests related to incoming connections.
#[derive(Encode, Decode, Debug)]
pub enum IncomingRequest {
    /// A request made by layer when it starts listening for mirrored connections.
    PortSubscribe(PortSubscribe),
    /// A request made by the layer when it closes the socket listening for mirrored connections.
    PortUnsubscribe(PortUnsubscribe),
}

/// A request to start proxying incoming connections.
///
/// For each connection incoming to the remote port,
/// the internal proxy will initiate a new connection to the local port specified in `listening_on`.
/// Through this connection, the proxy will first send the [`SocketAddress`] of the original peer,
/// serialized with [`crate::codec`]. After that, the proxy will send raw data.
#[derive(Encode, Decode, Debug, Clone)]
pub struct PortSubscribe {
    /// Port on the remote pod that layer wants want to mirror.
    pub port: Port,
    /// Local address on which the layer is listening.
    pub listening_on: SocketAddr,
}

/// A request to stop proxying incoming connections.
#[derive(Encode, Decode, Debug)]
pub struct PortUnsubscribe {
    /// Port on the remote pod that layer mirrored.
    pub port: Port,
}

/// Messages sent by the internal proxy and handled by the layer.
#[derive(Encode, Decode, Debug)]
pub enum ProxyToLayerMessage {
    /// A response to [`NewSessionRequest`]. Contains the identifier of the new `layer <-> proxy`
    /// session.
    NewSession(SessionId),
    /// A response to layer's [`FileRequest`].
    File(FileResponse),
    /// A response to layer's [`GetAddrInfoRequest`].
    GetAddrInfo(GetAddrInfoResponse),
    /// A response to layer's [`OutgoingConnectRequest`]
    OutgoingConnect(RemoteResult<OutgoingConnectResponse>),
    /// A response to layer's [`PortSubscribe`]
    IncomingSubscribe(RemoteResult<()>),
    /// A response to layer's [`PortUnsubscribe`]
    IncomingUbsubscribe(RemoteResult<()>),
    /// Agent log proxied to the layer.
    AgentLog(LogMessage),
}

/// A response to layer's [`OutgoingConnectRequest`].
#[derive(Encode, Decode, Debug)]
pub struct OutgoingConnectResponse {
    /// The address the layer should connect to instead of the address requested by the user.
    pub layer_address: SocketAddress,
    /// In-cluster address of the pod.
    pub in_cluster_address: SocketAddress,
}

/// A helper trait for `layer -> proxy` requests.
pub trait IsLayerRequest: Sized {
    /// Wraps this request so that it can be sent through the connection.
    fn wrap(self) -> LayerToProxyMessage;

    /// Tries to unwrap a request of this type from the message.
    /// On error, returns the message as it was.
    fn try_unwrap(message: LayerToProxyMessage) -> Result<Self, LayerToProxyMessage>;
}

/// A helper trait for `layer -> proxy` requests that require a response from the proxy.
/// Not all layer requests require a response, e.g.
/// [`CloseFileRequest`](mirrord_protocol::file::CloseFileRequest).
///
/// # Note
///
/// Instead of this, we should ideally have something like `IsProxyResponse` trait.
/// However, `proxy <-> agent` protocol uses the same response type for multiple requests.
/// Translating agent responses into unique types would generate a lot of boilerplate code.
pub trait IsLayerRequestWithResponse: IsLayerRequest {
    /// Type of response to this request.
    type Response: Sized;

    /// Wraps the response so that it can be sent through the connection.
    fn wrap_response(response: Self::Response) -> ProxyToLayerMessage;

    /// Tries to unwrap a response of valid type from the message.
    /// On error, returns the message as it was.
    fn try_unwrap_response(
        response: ProxyToLayerMessage,
    ) -> Result<Self::Response, ProxyToLayerMessage>;
}

impl_request!(
    req = OpenFileRequest,
    res = RemoteResult<OpenFileResponse>,
    req_path = LayerToProxyMessage::File => FileRequest::Open,
    res_path = ProxyToLayerMessage::File => FileResponse::Open,
);

impl_request!(
    req = OpenRelativeFileRequest,
    res = RemoteResult<OpenFileResponse>,
    req_path = LayerToProxyMessage::File => FileRequest::OpenRelative,
    res_path = ProxyToLayerMessage::File => FileResponse::Open,
);

impl_request!(
    req = ReadFileRequest,
    res = RemoteResult<ReadFileResponse>,
    req_path = LayerToProxyMessage::File => FileRequest::Read,
    res_path = ProxyToLayerMessage::File => FileResponse::Read,
);

impl_request!(
    req = ReadLimitedFileRequest,
    res = RemoteResult<ReadFileResponse>,
    req_path = LayerToProxyMessage::File => FileRequest::ReadLimited,
    res_path = ProxyToLayerMessage::File => FileResponse::ReadLimited,
);

impl_request!(
    req = SeekFileRequest,
    res = RemoteResult<SeekFileResponse>,
    req_path = LayerToProxyMessage::File => FileRequest::Seek,
    res_path = ProxyToLayerMessage::File => FileResponse::Seek,
);

impl_request!(
    req = WriteFileRequest,
    res = RemoteResult<WriteFileResponse>,
    req_path = LayerToProxyMessage::File => FileRequest::Write,
    res_path = ProxyToLayerMessage::File => FileResponse::Write,
);

impl_request!(
    req = WriteLimitedFileRequest,
    res = RemoteResult<WriteFileResponse>,
    req_path = LayerToProxyMessage::File => FileRequest::WriteLimited,
    res_path = ProxyToLayerMessage::File => FileResponse::WriteLimited,
);

impl_request!(
    req = AccessFileRequest,
    res = RemoteResult<AccessFileResponse>,
    req_path = LayerToProxyMessage::File => FileRequest::Access,
    res_path = ProxyToLayerMessage::File => FileResponse::Access,
);

impl_request!(
    req = XstatRequest,
    res = RemoteResult<XstatResponse>,
    req_path = LayerToProxyMessage::File => FileRequest::Xstat,
    res_path = ProxyToLayerMessage::File => FileResponse::Xstat,
);

impl_request!(
    req = XstatFsRequest,
    res = RemoteResult<XstatFsResponse>,
    req_path = LayerToProxyMessage::File => FileRequest::XstatFs,
    res_path = ProxyToLayerMessage::File => FileResponse::XstatFs,
);

impl_request!(
    req = FdOpenDirRequest,
    res = RemoteResult<OpenDirResponse>,
    req_path = LayerToProxyMessage::File => FileRequest::FdOpenDir,
    res_path = ProxyToLayerMessage::File => FileResponse::OpenDir,
);

impl_request!(
    req = ReadDirRequest,
    res = RemoteResult<ReadDirResponse>,
    req_path = LayerToProxyMessage::File => FileRequest::ReadDir,
    res_path = ProxyToLayerMessage::File => FileResponse::ReadDir,
);

impl_request!(
    req = GetDEnts64Request,
    res = RemoteResult<GetDEnts64Response>,
    req_path = LayerToProxyMessage::File => FileRequest::GetDEnts64,
    res_path = ProxyToLayerMessage::File => FileResponse::GetDEnts64,
);

impl_request!(
    req = CloseFileRequest,
    req_path = LayerToProxyMessage::File => FileRequest::Close,
);

impl_request!(
    req = CloseDirRequest,
    req_path = LayerToProxyMessage::File => FileRequest::CloseDir,
);

impl_request!(
    req = GetAddrInfoRequest,
    res = GetAddrInfoResponse,
    req_path = LayerToProxyMessage::GetAddrInfo,
    res_path = ProxyToLayerMessage::GetAddrInfo,
);

impl_request!(
    req = OutgoingConnectRequest,
    res = RemoteResult<OutgoingConnectResponse>,
    req_path = LayerToProxyMessage::OutgoingConnect,
    res_path = ProxyToLayerMessage::OutgoingConnect,
);

impl_request!(
    req = PortSubscribe,
    res = RemoteResult<()>,
    req_path = LayerToProxyMessage::Incoming => IncomingRequest::PortSubscribe,
    res_path = ProxyToLayerMessage::IncomingSubscribe,
);

impl_request!(
    req = PortUnsubscribe,
    res = RemoteResult<()>,
    req_path = LayerToProxyMessage::Incoming => IncomingRequest::PortUnsubscribe,
    res_path = ProxyToLayerMessage::IncomingUbsubscribe,
);
