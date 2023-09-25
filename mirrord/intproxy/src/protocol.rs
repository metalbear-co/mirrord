use bincode::{Decode, Encode};
use mirrord_protocol::FileResponse;

#[derive(Encode, Decode, Debug)]
pub struct LocalMessage<T> {
    pub message_id: u64,
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
    HookMessage(hook::HookMessage),
}

#[derive(Encode, Decode, Debug)]
pub enum ProxyToLayerMessage {
    SessionInfo(SessionId),
    FileResponse(FileResponse),
}

pub mod hook {
    use std::{
        borrow::Borrow,
        hash::{Hash, Hasher},
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
        path::PathBuf,
    };

    use bincode::{Decode, Encode};
    use mirrord_protocol::{
        file::{
            AccessFileResponse, GetDEnts64Response, OpenDirResponse, OpenFileResponse,
            OpenOptionsInternal, ReadDirResponse, ReadFileResponse, SeekFileResponse,
            SeekFromInternal, WriteFileResponse, XstatFsResponse, XstatResponse,
        },
        FileResponse, Port, RemoteResult,
    };

    use super::{HasResponse, LayerToProxyMessage, ProxyToLayerMessage};

    #[derive(Encode, Decode, Debug)]
    pub enum HookMessage {
        /// TCP incoming messages originating from a hook, see [`TcpIncoming`].
        Tcp(TcpIncoming),

        /// TCP outgoing messages originating from a hook, see [`TcpOutgoing`].
        // TcpOutgoing(TcpOutgoing),

        // /// UDP outgoing messages originating from a hook, see [`UdpOutgoing`].
        // UdpOutgoing(UdpOutgoing),

        /// File messages originating from a hook, see [`FileOperation`].
        File(FileOperation),
        // / Message originating from `getaddrinfo`, see [`GetAddrInfo`].
        // GetAddrinfo(GetAddrInfo),
    }

    #[derive(Encode, Decode, Debug)]
    pub enum TcpIncoming {
        Listen(Listen),
        Close(Port),
    }

    #[derive(Encode, Decode, Debug, Clone)]
    pub struct Listen {
        pub mirror_port: Port,
        pub requested_port: Port,
        pub ipv6: bool,
        pub id: u64,
    }

    impl PartialEq for Listen {
        fn eq(&self, other: &Self) -> bool {
            self.requested_port == other.requested_port
        }
    }

    impl Eq for Listen {}

    impl Hash for Listen {
        fn hash<H: Hasher>(&self, state: &mut H) {
            self.requested_port.hash(state);
        }
    }

    impl Borrow<Port> for Listen {
        fn borrow(&self) -> &Port {
            &self.requested_port
        }
    }

    impl From<&Listen> for SocketAddr {
        fn from(listen: &Listen) -> Self {
            let address = if listen.ipv6 {
                SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), listen.mirror_port)
            } else {
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), listen.mirror_port)
            };

            debug_assert_eq!(address.port(), listen.mirror_port);
            address
        }
    }

    #[derive(Encode, Decode, Debug)]
    pub struct Open {
        pub path: PathBuf,
        // pub(crate) file_channel_tx: ResponseChannel<OpenFileResponse>,
        pub open_options: OpenOptionsInternal,
    }

    #[derive(Encode, Decode, Debug)]
    pub struct OpenRelative {
        pub relative_fd: u64,
        pub path: PathBuf,
        // pub(crate) file_channel_tx: ResponseChannel<OpenFileResponse>,
        pub open_options: OpenOptionsInternal,
    }

    #[derive(Encode, Decode, Debug)]
    pub struct Read {
        pub remote_fd: u64,
        pub buffer_size: u64,
    }

    #[derive(Encode, Decode, Debug)]
    pub struct ReadLimited {
        pub remote_fd: u64,
        pub buffer_size: u64,
        pub start_from: u64,
    }

    #[derive(Encode, Decode, Debug)]
    pub struct Seek {
        pub remote_fd: u64,
        pub seek_from: SeekFromInternal,
        // pub(crate) file_channel_tx: ResponseChannel<SeekFileResponse>,
    }

    #[derive(Encode, Decode, Debug)]
    pub struct Write {
        pub remote_fd: u64,
        pub write_bytes: Vec<u8>,
    }

    #[derive(Encode, Decode, Debug)]
    pub struct WriteLimited {
        pub remote_fd: u64,
        pub write_bytes: Vec<u8>,
        pub start_from: u64,
    }

    #[derive(Encode, Decode, Debug)]
    pub struct Close {
        pub fd: u64,
    }

    #[derive(Encode, Decode, Debug)]
    pub struct Access {
        pub path: PathBuf,
        pub mode: u8,
    }

    pub type RemoteFd = u64;

    #[derive(Encode, Decode, Debug)]
    pub struct Xstat {
        pub path: Option<PathBuf>,
        pub fd: Option<RemoteFd>,
        pub follow_symlink: bool,
    }

    #[derive(Encode, Decode, Debug)]
    pub struct XstatFs {
        pub fd: RemoteFd,
    }

    #[derive(Encode, Decode, Debug)]
    pub struct ReadDir {
        pub remote_fd: u64,
    }

    #[derive(Encode, Decode, Debug)]
    pub struct FdOpenDir {
        pub remote_fd: u64,
    }

    #[derive(Encode, Decode, Debug)]
    pub struct CloseDir {
        pub fd: u64,
    }

    #[cfg(target_os = "linux")]
    #[derive(Encode, Decode, Debug)]
    pub struct GetDEnts64 {
        pub remote_fd: u64,
        pub buffer_size: u64,
    }

    #[derive(Encode, Decode, Debug)]
    pub enum FileOperation {
        Open(Open),
        OpenRelative(OpenRelative),
        Read(Read),
        ReadLimited(ReadLimited),
        Write(Write),
        WriteLimited(WriteLimited),
        Seek(Seek),
        Close(Close),
        Access(Access),
        Xstat(Xstat),
        XstatFs(XstatFs),
        ReadDir(ReadDir),
        FdOpenDir(FdOpenDir),
        CloseDir(CloseDir),
        #[cfg(target_os = "linux")]
        GetDEnts64(GetDEnts64),
    }

    impl HasResponse for Open {
        type Response = RemoteResult<OpenFileResponse>;

        fn into_message(self) -> LayerToProxyMessage {
            LayerToProxyMessage::HookMessage(HookMessage::File(FileOperation::Open(self)))
        }

        fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
            ProxyToLayerMessage::FileResponse(FileResponse::Open(response))
        }

        fn unwrap_response(
            message: ProxyToLayerMessage,
        ) -> core::result::Result<Self::Response, ProxyToLayerMessage> {
            match message {
                ProxyToLayerMessage::FileResponse(FileResponse::Open(response)) => Ok(response),
                other => Err(other),
            }
        }
    }

    impl HasResponse for Read {
        type Response = RemoteResult<ReadFileResponse>;

        fn into_message(self) -> LayerToProxyMessage {
            LayerToProxyMessage::HookMessage(HookMessage::File(FileOperation::Read(self)))
        }

        fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
            ProxyToLayerMessage::FileResponse(FileResponse::Read(response))
        }

        fn unwrap_response(
            message: ProxyToLayerMessage,
        ) -> core::result::Result<Self::Response, ProxyToLayerMessage> {
            match message {
                ProxyToLayerMessage::FileResponse(FileResponse::Read(response)) => Ok(response),
                other => Err(other),
            }
        }
    }

    impl HasResponse for ReadLimited {
        type Response = RemoteResult<ReadFileResponse>;

        fn into_message(self) -> LayerToProxyMessage {
            LayerToProxyMessage::HookMessage(HookMessage::File(FileOperation::ReadLimited(self)))
        }

        fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
            ProxyToLayerMessage::FileResponse(FileResponse::ReadLimited(response))
        }

        fn unwrap_response(
            message: ProxyToLayerMessage,
        ) -> core::result::Result<Self::Response, ProxyToLayerMessage> {
            match message {
                ProxyToLayerMessage::FileResponse(FileResponse::ReadLimited(response)) => {
                    Ok(response)
                }
                other => Err(other),
            }
        }
    }

    impl HasResponse for FdOpenDir {
        type Response = RemoteResult<OpenDirResponse>;

        fn into_message(self) -> LayerToProxyMessage {
            LayerToProxyMessage::HookMessage(HookMessage::File(FileOperation::FdOpenDir(self)))
        }

        fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
            ProxyToLayerMessage::FileResponse(FileResponse::OpenDir(response))
        }

        fn unwrap_response(
            message: ProxyToLayerMessage,
        ) -> core::result::Result<Self::Response, ProxyToLayerMessage> {
            match message {
                ProxyToLayerMessage::FileResponse(FileResponse::OpenDir(response)) => Ok(response),
                other => Err(other),
            }
        }
    }

    impl HasResponse for ReadDir {
        type Response = RemoteResult<ReadDirResponse>;

        fn into_message(self) -> LayerToProxyMessage {
            LayerToProxyMessage::HookMessage(HookMessage::File(FileOperation::ReadDir(self)))
        }

        fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
            ProxyToLayerMessage::FileResponse(FileResponse::ReadDir(response))
        }

        fn unwrap_response(
            message: ProxyToLayerMessage,
        ) -> core::result::Result<Self::Response, ProxyToLayerMessage> {
            match message {
                ProxyToLayerMessage::FileResponse(FileResponse::ReadDir(response)) => Ok(response),
                other => Err(other),
            }
        }
    }

    impl HasResponse for OpenRelative {
        type Response = RemoteResult<OpenFileResponse>;

        fn into_message(self) -> LayerToProxyMessage {
            LayerToProxyMessage::HookMessage(HookMessage::File(FileOperation::OpenRelative(self)))
        }

        fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
            ProxyToLayerMessage::FileResponse(FileResponse::Open(response))
        }

        fn unwrap_response(
            message: ProxyToLayerMessage,
        ) -> core::result::Result<Self::Response, ProxyToLayerMessage> {
            match message {
                ProxyToLayerMessage::FileResponse(FileResponse::Open(response)) => Ok(response),
                other => Err(other),
            }
        }
    }

    impl HasResponse for Write {
        type Response = RemoteResult<WriteFileResponse>;

        fn into_message(self) -> LayerToProxyMessage {
            LayerToProxyMessage::HookMessage(HookMessage::File(FileOperation::Write(self)))
        }

        fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
            ProxyToLayerMessage::FileResponse(FileResponse::Write(response))
        }

        fn unwrap_response(
            message: ProxyToLayerMessage,
        ) -> core::result::Result<Self::Response, ProxyToLayerMessage> {
            match message {
                ProxyToLayerMessage::FileResponse(FileResponse::Write(response)) => Ok(response),
                other => Err(other),
            }
        }
    }

    impl HasResponse for WriteLimited {
        type Response = RemoteResult<WriteFileResponse>;

        fn into_message(self) -> LayerToProxyMessage {
            LayerToProxyMessage::HookMessage(HookMessage::File(FileOperation::WriteLimited(self)))
        }

        fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
            ProxyToLayerMessage::FileResponse(FileResponse::WriteLimited(response))
        }

        fn unwrap_response(
            message: ProxyToLayerMessage,
        ) -> core::result::Result<Self::Response, ProxyToLayerMessage> {
            match message {
                ProxyToLayerMessage::FileResponse(FileResponse::WriteLimited(response)) => {
                    Ok(response)
                }
                other => Err(other),
            }
        }
    }

    impl HasResponse for Seek {
        type Response = RemoteResult<SeekFileResponse>;

        fn into_message(self) -> LayerToProxyMessage {
            LayerToProxyMessage::HookMessage(HookMessage::File(FileOperation::Seek(self)))
        }

        fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
            ProxyToLayerMessage::FileResponse(FileResponse::Seek(response))
        }

        fn unwrap_response(
            message: ProxyToLayerMessage,
        ) -> core::result::Result<Self::Response, ProxyToLayerMessage> {
            match message {
                ProxyToLayerMessage::FileResponse(FileResponse::Seek(response)) => Ok(response),
                other => Err(other),
            }
        }
    }

    impl HasResponse for Access {
        type Response = RemoteResult<AccessFileResponse>;

        fn into_message(self) -> LayerToProxyMessage {
            LayerToProxyMessage::HookMessage(HookMessage::File(FileOperation::Access(self)))
        }

        fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
            ProxyToLayerMessage::FileResponse(FileResponse::Access(response))
        }

        fn unwrap_response(
            message: ProxyToLayerMessage,
        ) -> core::result::Result<Self::Response, ProxyToLayerMessage> {
            match message {
                ProxyToLayerMessage::FileResponse(FileResponse::Access(response)) => Ok(response),
                other => Err(other),
            }
        }
    }

    impl HasResponse for Xstat {
        type Response = RemoteResult<XstatResponse>;

        fn into_message(self) -> LayerToProxyMessage {
            LayerToProxyMessage::HookMessage(HookMessage::File(FileOperation::Xstat(self)))
        }

        fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
            ProxyToLayerMessage::FileResponse(FileResponse::Xstat(response))
        }

        fn unwrap_response(
            message: ProxyToLayerMessage,
        ) -> core::result::Result<Self::Response, ProxyToLayerMessage> {
            match message {
                ProxyToLayerMessage::FileResponse(FileResponse::Xstat(response)) => Ok(response),
                other => Err(other),
            }
        }
    }

    impl HasResponse for XstatFs {
        type Response = RemoteResult<XstatFsResponse>;

        fn into_message(self) -> LayerToProxyMessage {
            LayerToProxyMessage::HookMessage(HookMessage::File(FileOperation::XstatFs(self)))
        }

        fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
            ProxyToLayerMessage::FileResponse(FileResponse::XstatFs(response))
        }

        fn unwrap_response(
            message: ProxyToLayerMessage,
        ) -> core::result::Result<Self::Response, ProxyToLayerMessage> {
            match message {
                ProxyToLayerMessage::FileResponse(FileResponse::XstatFs(response)) => Ok(response),
                other => Err(other),
            }
        }
    }

    impl HasResponse for GetDEnts64 {
        type Response = RemoteResult<GetDEnts64Response>;

        fn into_message(self) -> LayerToProxyMessage {
            LayerToProxyMessage::HookMessage(HookMessage::File(FileOperation::GetDEnts64(self)))
        }

        fn wrap_response(response: Self::Response) -> ProxyToLayerMessage {
            ProxyToLayerMessage::FileResponse(FileResponse::GetDEnts64(response))
        }

        fn unwrap_response(
            message: ProxyToLayerMessage,
        ) -> core::result::Result<Self::Response, ProxyToLayerMessage> {
            match message {
                ProxyToLayerMessage::FileResponse(FileResponse::GetDEnts64(response)) => {
                    Ok(response)
                }
                other => Err(other),
            }
        }
    }
}

pub trait HasResponse: Sized {
    type Response: Sized;

    fn into_message(self) -> LayerToProxyMessage;

    fn wrap_response(response: Self::Response) -> ProxyToLayerMessage;

    fn unwrap_response(
        message: ProxyToLayerMessage,
    ) -> core::result::Result<Self::Response, ProxyToLayerMessage>;
}
