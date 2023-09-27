use std::{fmt, marker::PhantomData};

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
    FileRequest, FileResponse, RemoteResult,
};

pub type MessageId = u64;

#[derive(Encode, Decode, Debug)]
pub struct LocalMessage<T> {
    pub message_id: MessageId,
    pub inner: T,
}

pub type SessionId = u64;

#[derive(Encode, Decode, Debug)]
pub enum InitSession {
    New,
    ForkedFrom(SessionId),
    ExecFrom(SessionId),
}

pub trait NetProtocol: fmt::Debug + Encode + Decode {}

#[derive(Debug, Encode, Decode)]
pub struct Tcp;

impl NetProtocol for Tcp {}

#[derive(Debug, Encode, Decode)]
pub struct Udp;

impl NetProtocol for Udp {}

#[derive(Encode, Decode, Debug)]
pub enum LayerToProxyMessage {
    InitSession(InitSession),

    File(FileRequest),

    GetAddrInfo(GetAddrInfoRequest),

    ConnectTcpOutgoing(ConnectOutgoing<Tcp>),
    ConnectUdpOutgoing(ConnectOutgoing<Udp>),
}

#[derive(Encode, Decode, Debug)]
pub struct ConnectOutgoing<P> {
    pub remote_address: SocketAddress,
    _phantom: PhantomData<fn() -> P>,
}

impl<P> ConnectOutgoing<P> {
    pub fn new(remote_address: SocketAddress) -> Self {
        Self {
            remote_address,
            _phantom: Default::default(),
        }
    }
}

#[derive(Encode, Decode, Debug)]
pub enum ProxyToLayerMessage {
    SessionInfo(SessionId),

    File(FileResponse),

    GetAddrInfo(GetAddrInfoResponse),

    ConnectTcpOutgoing(RemoteResult<OutgoingConnectResponse>),
    ConnectUdpOutgoing(RemoteResult<OutgoingConnectResponse>),
}

#[derive(Encode, Decode, Debug)]
pub struct OutgoingConnectResponse {
    pub layer_address: SocketAddress,
    pub user_app_address: SocketAddress,
}

pub trait IsLayerRequest: Sized {
    fn wrap(self) -> LayerToProxyMessage;

    fn check(message: &LayerToProxyMessage) -> bool;

    fn try_unwrap(message: LayerToProxyMessage) -> Result<Self, LayerToProxyMessage>;
}

pub trait IsLayerRequestWithResponse: IsLayerRequest {
    type Response: Sized;

    fn wrap_response(response: Self::Response) -> ProxyToLayerMessage;

    fn check_response(response: &ProxyToLayerMessage) -> bool;

    fn try_unwrap_response(
        response: ProxyToLayerMessage,
    ) -> Result<Self::Response, ProxyToLayerMessage>;
}

macro_rules! combine_paths {
    ($value: ident, $variant: path, $($rest: path),+) => {
        $variant(combine_paths!($value, $($rest),+))
    };

    ($value: ident, $variant: path) => { $variant($value) };
}

macro_rules! impl_request {
    (
        req = $req_type: path,
        res = $res_type: path,
        req_path = $($req_variants: path) => +,
        res_path = $($res_variants: path) => +,
    ) => {
        impl_request!(
            req = $req_type,
            req_path = $($req_variants) => +,
        );

        impl IsLayerRequestWithResponse for $req_type {
            type Response = $res_type;

            fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
                combine_paths!(response, $($res_variants),+)
            }

            fn check_response(response: &ProxyToLayerMessage) -> bool {
                match response {
                    combine_paths!(_inner, $($res_variants),+) => true,
                    _ => false,
                }
            }

            fn try_unwrap_response(response: ProxyToLayerMessage) -> Result<Self::Response, ProxyToLayerMessage> {
                match response {
                    combine_paths!(inner, $($res_variants),+) => Ok(inner),
                    other => Err(other),
                }
            }
        }
    };

    (
        req = $req_type: path,
        req_path = $($req_variants: path) => +,
    ) => {
        impl IsLayerRequest for $req_type {
            fn wrap(self) -> LayerToProxyMessage {
                combine_paths!(self, $($req_variants),+)
            }

            fn check(message: &LayerToProxyMessage) -> bool {
                match message {
                    combine_paths!(_inner, $($req_variants),+) => true,
                    _ => false,
                }
            }

            fn try_unwrap(message: LayerToProxyMessage) -> Result<Self, LayerToProxyMessage> {
                match message {
                    combine_paths!(inner, $($req_variants),+) => Ok(inner),
                    other => Err(other),
                }
            }
        }
    };
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
    req = ConnectOutgoing<Tcp>,
    res = RemoteResult<OutgoingConnectResponse>,
    req_path = LayerToProxyMessage::ConnectTcpOutgoing,
    res_path = ProxyToLayerMessage::ConnectTcpOutgoing,
);

impl_request!(
    req = ConnectOutgoing<Udp>,
    res = RemoteResult<OutgoingConnectResponse>,
    req_path = LayerToProxyMessage::ConnectUdpOutgoing,
    res_path = ProxyToLayerMessage::ConnectUdpOutgoing,
);
