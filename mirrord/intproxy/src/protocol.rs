use bincode::{Decode, Encode};
use mirrord_protocol::{
    file::{
        AccessFileRequest, AccessFileResponse, CloseDirRequest, CloseFileRequest, FdOpenDirRequest,
        GetDEnts64Request, GetDEnts64Response, OpenDirResponse, OpenFileRequest, OpenFileResponse,
        OpenRelativeFileRequest, ReadDirRequest, ReadDirResponse, ReadFileRequest,
        ReadFileResponse, ReadLimitedFileRequest, SeekFileRequest, SeekFileResponse,
        WriteFileRequest, WriteFileResponse, WriteLimitedFileRequest, XstatFsRequest,
        XstatFsResponse, XstatRequest, XstatResponse,
    },
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
    /// Raw requests from the layer
    LayerRequest(ClientMessage),
}

#[derive(Encode, Decode, Debug)]
pub enum ProxyToLayerMessage {
    SessionInfo(SessionId),
    /// Raw responses from the agent
    AgentResponse(AgentResponse),
}

#[derive(Encode, Decode, Debug)]
pub enum AgentResponse {
    File(FileResponse),
}

pub trait IsLayerRequest: Sized {
    fn wrap(self) -> ClientMessage;

    fn try_unwrap(message: ClientMessage) -> Result<Self, ClientMessage>;
}

pub trait HasResponse: IsLayerRequest {
    type Response: Sized;

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

        impl HasResponse for $req_type {
            type Response = $res_type;

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
            fn wrap(self) -> ClientMessage {
                combine_paths!(self, $($req_variants),+)
            }

            fn try_unwrap(message: ClientMessage) -> Result<Self, ClientMessage> {
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
    req_path = ClientMessage::FileRequest => FileRequest::Open,
    res_path = AgentResponse::File => FileResponse::Open,
);

impl_request!(
    req = OpenRelativeFileRequest,
    res = RemoteResult<OpenFileResponse>,
    req_path = ClientMessage::FileRequest => FileRequest::OpenRelative,
    res_path = AgentResponse::File => FileResponse::Open,
);

impl_request!(
    req = ReadFileRequest,
    res = RemoteResult<ReadFileResponse>,
    req_path = ClientMessage::FileRequest => FileRequest::Read,
    res_path = AgentResponse::File => FileResponse::Read,
);

impl_request!(
    req = ReadLimitedFileRequest,
    res = RemoteResult<ReadFileResponse>,
    req_path = ClientMessage::FileRequest => FileRequest::ReadLimited,
    res_path = AgentResponse::File => FileResponse::ReadLimited,
);

impl_request!(
    req = SeekFileRequest,
    res = RemoteResult<SeekFileResponse>,
    req_path = ClientMessage::FileRequest => FileRequest::Seek,
    res_path = AgentResponse::File => FileResponse::Seek,
);

impl_request!(
    req = WriteFileRequest,
    res = RemoteResult<WriteFileResponse>,
    req_path = ClientMessage::FileRequest => FileRequest::Write,
    res_path = AgentResponse::File => FileResponse::Write,
);

impl_request!(
    req = WriteLimitedFileRequest,
    res = RemoteResult<WriteFileResponse>,
    req_path = ClientMessage::FileRequest => FileRequest::WriteLimited,
    res_path = AgentResponse::File => FileResponse::WriteLimited,
);

impl_request!(
    req = AccessFileRequest,
    res = RemoteResult<AccessFileResponse>,
    req_path = ClientMessage::FileRequest => FileRequest::Access,
    res_path = AgentResponse::File => FileResponse::Access,
);

impl_request!(
    req = XstatRequest,
    res = RemoteResult<XstatResponse>,
    req_path = ClientMessage::FileRequest => FileRequest::Xstat,
    res_path = AgentResponse::File => FileResponse::Xstat,
);

impl_request!(
    req = XstatFsRequest,
    res = RemoteResult<XstatFsResponse>,
    req_path = ClientMessage::FileRequest => FileRequest::XstatFs,
    res_path = AgentResponse::File => FileResponse::XstatFs,
);

impl_request!(
    req = FdOpenDirRequest,
    res = RemoteResult<OpenDirResponse>,
    req_path = ClientMessage::FileRequest => FileRequest::FdOpenDir,
    res_path = AgentResponse::File => FileResponse::OpenDir,
);

impl_request!(
    req = ReadDirRequest,
    res = RemoteResult<ReadDirResponse>,
    req_path = ClientMessage::FileRequest => FileRequest::ReadDir,
    res_path = AgentResponse::File => FileResponse::ReadDir,
);

impl_request!(
    req = GetDEnts64Request,
    res = RemoteResult<GetDEnts64Response>,
    req_path = ClientMessage::FileRequest => FileRequest::GetDEnts64,
    res_path = AgentResponse::File => FileResponse::GetDEnts64,
);

impl_request!(
    req = CloseFileRequest,
    req_path = ClientMessage::FileRequest => FileRequest::Close,
);

impl_request!(
    req = CloseDirRequest,
    req_path = ClientMessage::FileRequest => FileRequest::CloseDir,
);
