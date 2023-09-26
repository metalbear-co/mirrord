use std::marker::PhantomData;

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
    outgoing::{tcp::LayerTcpOutgoing, udp::LayerUdpOutgoing, LayerConnect, SocketAddress},
    ClientMessage, FileRequest, FileResponse, RemoteResult,
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

#[derive(Encode, Decode, Debug)]
pub enum LayerToProxyMessage {
    InitSession(InitSession),
    LayerRequest(LayerRequest),
}

#[derive(Encode, Decode, Debug)]
pub enum LayerRequest {
    File(FileRequest),
    GetAddrInfo(GetAddrInfoRequest),
    ConnectTcpOutgoing(ConnectTcpOutgoing),
    ConnectUdpOutgoing(ConnectUdpOutgoing),
}

impl From<LayerRequest> for ClientMessage {
    fn from(value: LayerRequest) -> Self {
        match value {
            LayerRequest::File(req) => ClientMessage::FileRequest(req),
            LayerRequest::GetAddrInfo(req) => ClientMessage::GetAddrInfoRequest(req),
            LayerRequest::ConnectTcpOutgoing(req) => {
                ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
                    remote_address: req.remote_address,
                }))
            }
            LayerRequest::ConnectUdpOutgoing(req) => {
                ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect {
                    remote_address: req.remote_address,
                }))
            }
        }
    }
}

#[derive(Encode, Decode, Debug)]
pub struct ConnectTcpOutgoing<P> {
    pub remote_address: SocketAddress,
    _phantom: PhantomData<fn() -> P>,
}

#[derive(Encode, Decode, Debug)]
pub struct ConnectUdpOutgoing {
    pub remote_address: SocketAddress,
}

#[derive(Encode, Decode, Debug)]
pub enum ProxyToLayerMessage {
    SessionInfo(SessionId),
    AgentResponse(AgentResponse),
}

#[derive(Encode, Decode, Debug)]
pub enum AgentResponse {
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
    fn wrap(self) -> LayerRequest;

    fn check(message: &LayerRequest) -> bool;

    fn try_unwrap(message: LayerRequest) -> Result<Self, LayerRequest>;
}

pub trait IsLayerRequestWithResponse: IsLayerRequest {
    type Response: Sized;

    fn wrap_response(response: Self::Response) -> AgentResponse;

    fn check_response(response: &AgentResponse) -> bool;

    fn try_unwrap_response(response: AgentResponse) -> Result<Self::Response, AgentResponse>;
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

            fn wrap_response(response: Self::Response) -> AgentResponse {
                combine_paths!(response, $($res_variants),+)
            }

            fn check_response(response: &AgentResponse) -> bool {
                match response {
                    combine_paths!(_inner, $($res_variants),+) => true,
                    _ => false,
                }
            }

            fn try_unwrap_response(response: AgentResponse) -> Result<Self::Response, AgentResponse> {
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
            fn wrap(self) -> LayerRequest {
                combine_paths!(self, $($req_variants),+)
            }

            fn check(message: &LayerRequest) -> bool {
                match message {
                    combine_paths!(_inner, $($req_variants),+) => true,
                    _ => false,
                }
            }

            fn try_unwrap(message: LayerRequest) -> Result<Self, LayerRequest> {
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
    req_path = LayerRequest::File => FileRequest::Open,
    res_path = AgentResponse::File => FileResponse::Open,
);

impl_request!(
    req = OpenRelativeFileRequest,
    res = RemoteResult<OpenFileResponse>,
    req_path = LayerRequest::File => FileRequest::OpenRelative,
    res_path = AgentResponse::File => FileResponse::Open,
);

impl_request!(
    req = ReadFileRequest,
    res = RemoteResult<ReadFileResponse>,
    req_path = LayerRequest::File => FileRequest::Read,
    res_path = AgentResponse::File => FileResponse::Read,
);

impl_request!(
    req = ReadLimitedFileRequest,
    res = RemoteResult<ReadFileResponse>,
    req_path = LayerRequest::File => FileRequest::ReadLimited,
    res_path = AgentResponse::File => FileResponse::ReadLimited,
);

impl_request!(
    req = SeekFileRequest,
    res = RemoteResult<SeekFileResponse>,
    req_path = LayerRequest::File => FileRequest::Seek,
    res_path = AgentResponse::File => FileResponse::Seek,
);

impl_request!(
    req = WriteFileRequest,
    res = RemoteResult<WriteFileResponse>,
    req_path = LayerRequest::File => FileRequest::Write,
    res_path = AgentResponse::File => FileResponse::Write,
);

impl_request!(
    req = WriteLimitedFileRequest,
    res = RemoteResult<WriteFileResponse>,
    req_path = LayerRequest::File => FileRequest::WriteLimited,
    res_path = AgentResponse::File => FileResponse::WriteLimited,
);

impl_request!(
    req = AccessFileRequest,
    res = RemoteResult<AccessFileResponse>,
    req_path = LayerRequest::File => FileRequest::Access,
    res_path = AgentResponse::File => FileResponse::Access,
);

impl_request!(
    req = XstatRequest,
    res = RemoteResult<XstatResponse>,
    req_path = LayerRequest::File => FileRequest::Xstat,
    res_path = AgentResponse::File => FileResponse::Xstat,
);

impl_request!(
    req = XstatFsRequest,
    res = RemoteResult<XstatFsResponse>,
    req_path = LayerRequest::File => FileRequest::XstatFs,
    res_path = AgentResponse::File => FileResponse::XstatFs,
);

impl_request!(
    req = FdOpenDirRequest,
    res = RemoteResult<OpenDirResponse>,
    req_path = LayerRequest::File => FileRequest::FdOpenDir,
    res_path = AgentResponse::File => FileResponse::OpenDir,
);

impl_request!(
    req = ReadDirRequest,
    res = RemoteResult<ReadDirResponse>,
    req_path = LayerRequest::File => FileRequest::ReadDir,
    res_path = AgentResponse::File => FileResponse::ReadDir,
);

impl_request!(
    req = GetDEnts64Request,
    res = RemoteResult<GetDEnts64Response>,
    req_path = LayerRequest::File => FileRequest::GetDEnts64,
    res_path = AgentResponse::File => FileResponse::GetDEnts64,
);

impl_request!(
    req = CloseFileRequest,
    req_path = LayerRequest::File => FileRequest::Close,
);

impl_request!(
    req = CloseDirRequest,
    req_path = LayerRequest::File => FileRequest::CloseDir,
);

impl_request!(
    req = GetAddrInfoRequest,
    res = GetAddrInfoResponse,
    req_path = LayerRequest::GetAddrInfo,
    res_path = AgentResponse::GetAddrInfo,
);

impl_request!(
    req = ConnectTcpOutgoing,
    res = RemoteResult<OutgoingConnectResponse>,
    req_path = LayerRequest::ConnectTcpOutgoing,
    res_path = AgentResponse::ConnectTcpOutgoing,
);

impl_request!(
    req = ConnectUdpOutgoing,
    res = RemoteResult<OutgoingConnectResponse>,
    req_path = LayerRequest::ConnectUdpOutgoing,
    res_path = AgentResponse::ConnectUdpOutgoing,
);
