use mirrord_protocol::{
    DaemonMessage, FileRequest, FileResponse,
    file::{
        AccessFileRequest, CloseDirRequest, CloseFileRequest, FchmodRequest, FchownRequest,
        FdOpenDirRequest, FtruncateRequest, FutimensRequest, GetDEnts64Request, GetDEnts64Response,
        MakeDirAtRequest, MakeDirRequest, OpenDirResponse, OpenFileRequest, OpenFileResponse,
        OpenRelativeFileRequest, ReadDirBatchRequest, ReadDirBatchResponse, ReadDirRequest,
        ReadFileRequest, ReadLimitedFileRequest, ReadLinkFileRequest, RemoveDirRequest,
        RenameRequest, SeekFileRequest, StatFsRequest, StatFsRequestV2, UnlinkAtRequest,
        UnlinkRequest, WriteFileRequest, WriteLimitedFileRequest, XstatFsRequest, XstatFsRequestV2,
        XstatRequest,
    },
};

/// Convenience trait for [`mirrord_protocol`] file requests.
pub trait FileRequestExt {
    /// If this request refers to a remote file descriptor, returns a mutable reference to it.
    fn remote_fd_mut(&mut self) -> Option<&mut u64>;

    /// If this request refers to a remote file descriptor, returns it.
    fn remote_fd(&self) -> Option<u64>;

    /// Transforms this request into [`FileRequest`].
    fn into_file_request(self) -> FileRequest;
}

impl FileRequestExt for FileRequest {
    fn remote_fd_mut(&mut self) -> Option<&mut u64> {
        match self {
            FileRequest::Open(req) => req.remote_fd_mut(),
            FileRequest::OpenRelative(req) => req.remote_fd_mut(),
            FileRequest::Read(req) => req.remote_fd_mut(),
            FileRequest::ReadLimited(req) => req.remote_fd_mut(),
            FileRequest::Seek(req) => req.remote_fd_mut(),
            FileRequest::Write(req) => req.remote_fd_mut(),
            FileRequest::WriteLimited(req) => req.remote_fd_mut(),
            FileRequest::Close(req) => req.remote_fd_mut(),
            FileRequest::Access(req) => req.remote_fd_mut(),
            FileRequest::Xstat(req) => req.remote_fd_mut(),
            FileRequest::XstatFs(req) => req.remote_fd_mut(),
            FileRequest::FdOpenDir(req) => req.remote_fd_mut(),
            FileRequest::ReadDir(req) => req.remote_fd_mut(),
            FileRequest::CloseDir(req) => req.remote_fd_mut(),
            FileRequest::GetDEnts64(req) => req.remote_fd_mut(),
            FileRequest::ReadLink(req) => req.remote_fd_mut(),
            FileRequest::ReadDirBatch(req) => req.remote_fd_mut(),
            FileRequest::MakeDir(req) => req.remote_fd_mut(),
            FileRequest::MakeDirAt(req) => req.remote_fd_mut(),
            FileRequest::RemoveDir(req) => req.remote_fd_mut(),
            FileRequest::Unlink(req) => req.remote_fd_mut(),
            FileRequest::UnlinkAt(req) => req.remote_fd_mut(),
            FileRequest::StatFs(req) => req.remote_fd_mut(),
            FileRequest::XstatFsV2(req) => req.remote_fd_mut(),
            FileRequest::StatFsV2(req) => req.remote_fd_mut(),
            FileRequest::Rename(req) => req.remote_fd_mut(),
            FileRequest::Ftruncate(req) => req.remote_fd_mut(),
            FileRequest::Futimens(req) => req.remote_fd_mut(),
            FileRequest::Fchown(req) => req.remote_fd_mut(),
            FileRequest::Fchmod(req) => req.remote_fd_mut(),
        }
    }

    fn remote_fd(&self) -> Option<u64> {
        match self {
            FileRequest::Open(req) => req.remote_fd(),
            FileRequest::OpenRelative(req) => req.remote_fd(),
            FileRequest::Read(req) => req.remote_fd(),
            FileRequest::ReadLimited(req) => req.remote_fd(),
            FileRequest::Seek(req) => req.remote_fd(),
            FileRequest::Write(req) => req.remote_fd(),
            FileRequest::WriteLimited(req) => req.remote_fd(),
            FileRequest::Close(req) => req.remote_fd(),
            FileRequest::Access(req) => req.remote_fd(),
            FileRequest::Xstat(req) => req.remote_fd(),
            FileRequest::XstatFs(req) => req.remote_fd(),
            FileRequest::FdOpenDir(req) => req.remote_fd(),
            FileRequest::ReadDir(req) => req.remote_fd(),
            FileRequest::CloseDir(req) => req.remote_fd(),
            FileRequest::GetDEnts64(req) => req.remote_fd(),
            FileRequest::ReadLink(req) => req.remote_fd(),
            FileRequest::ReadDirBatch(req) => req.remote_fd(),
            FileRequest::MakeDir(req) => req.remote_fd(),
            FileRequest::MakeDirAt(req) => req.remote_fd(),
            FileRequest::RemoveDir(req) => req.remote_fd(),
            FileRequest::Unlink(req) => req.remote_fd(),
            FileRequest::UnlinkAt(req) => req.remote_fd(),
            FileRequest::StatFs(req) => req.remote_fd(),
            FileRequest::XstatFsV2(req) => req.remote_fd(),
            FileRequest::StatFsV2(req) => req.remote_fd(),
            FileRequest::Rename(req) => req.remote_fd(),
            FileRequest::Ftruncate(req) => req.remote_fd(),
            FileRequest::Futimens(req) => req.remote_fd(),
            FileRequest::Fchown(req) => req.remote_fd(),
            FileRequest::Fchmod(req) => req.remote_fd(),
        }
    }

    fn into_file_request(self) -> FileRequest {
        self
    }
}

/// Implements [`FileRequestExt`] for a request.
macro_rules! impl_file_request_ext {
    // No remote fd.
    ($request:ty, $variant:ident) => {
        impl FileRequestExt for $request {
            fn remote_fd_mut(&mut self) -> Option<&mut u64> {
                None
            }

            fn remote_fd(&self) -> Option<u64> {
                None
            }

            fn into_file_request(self) -> FileRequest {
                FileRequest::$variant(self)
            }
        }
    };

    // Optional or non-optional remote fd.
    ($request:ty, $variant:ident, $field:ident) => {
        impl FileRequestExt for $request {
            fn remote_fd_mut(&mut self) -> Option<&mut u64> {
                self.$field.get_mut()
            }

            fn remote_fd(&self) -> Option<u64> {
                self.$field.get()
            }

            fn into_file_request(self) -> FileRequest {
                FileRequest::$variant(self)
            }
        }
    };
}

/// Helper trait for [`impl_file_request_ext`].
trait HasRemoteFd {
    fn get_mut(&mut self) -> Option<&mut u64>;

    fn get(self) -> Option<u64>;
}

impl HasRemoteFd for Option<u64> {
    fn get_mut(&mut self) -> Option<&mut u64> {
        self.as_mut()
    }

    fn get(self) -> Option<u64> {
        self
    }
}

impl HasRemoteFd for u64 {
    fn get_mut(&mut self) -> Option<&mut u64> {
        Some(self)
    }

    fn get(self) -> Option<u64> {
        Some(self)
    }
}

impl_file_request_ext!(OpenFileRequest, Open);
impl_file_request_ext!(OpenRelativeFileRequest, OpenRelative, relative_fd);
impl_file_request_ext!(ReadFileRequest, Read, remote_fd);
impl_file_request_ext!(ReadLimitedFileRequest, ReadLimited, remote_fd);
impl_file_request_ext!(SeekFileRequest, Seek, fd);
impl_file_request_ext!(WriteFileRequest, Write, fd);
impl_file_request_ext!(WriteLimitedFileRequest, WriteLimited, remote_fd);
impl_file_request_ext!(AccessFileRequest, Access);
impl_file_request_ext!(XstatFsRequest, XstatFs, fd);
impl_file_request_ext!(FdOpenDirRequest, FdOpenDir, remote_fd);
impl_file_request_ext!(ReadDirRequest, ReadDir, remote_fd);
impl_file_request_ext!(GetDEnts64Request, GetDEnts64, remote_fd);
impl_file_request_ext!(ReadLinkFileRequest, ReadLink);
impl_file_request_ext!(ReadDirBatchRequest, ReadDirBatch, remote_fd);
impl_file_request_ext!(MakeDirRequest, MakeDir);
impl_file_request_ext!(MakeDirAtRequest, MakeDirAt, dirfd);
impl_file_request_ext!(RemoveDirRequest, RemoveDir);
impl_file_request_ext!(UnlinkRequest, Unlink);
impl_file_request_ext!(UnlinkAtRequest, UnlinkAt, dirfd);
impl_file_request_ext!(StatFsRequest, StatFs);
impl_file_request_ext!(XstatFsRequestV2, XstatFsV2, fd);
impl_file_request_ext!(XstatRequest, Xstat, fd);
impl_file_request_ext!(StatFsRequestV2, StatFsV2);
impl_file_request_ext!(RenameRequest, Rename);
impl_file_request_ext!(FtruncateRequest, Ftruncate, fd);
impl_file_request_ext!(FutimensRequest, Futimens, fd);
impl_file_request_ext!(FchownRequest, Fchown, fd);
impl_file_request_ext!(FchmodRequest, Fchmod, fd);
impl_file_request_ext!(CloseFileRequest, Close, fd);
impl_file_request_ext!(CloseDirRequest, CloseDir, remote_fd);

/// Convenience trait for [`FileResponse`].
pub trait FileResponseExt {
    /// If this response refers to a remote file descriptor, returns a mutable reference to it.
    fn remote_fd_mut(&mut self) -> Option<&mut u64>;
}

impl FileResponseExt for FileResponse {
    fn remote_fd_mut(&mut self) -> Option<&mut u64> {
        // Matching on all variants explicitly here.
        // We want to trigger a compilation error if we add a new variant and forget to update this.
        match self {
            FileResponse::Access(..)
            | FileResponse::Read(..)
            | FileResponse::ReadLimited(..)
            | FileResponse::ReadDir(..)
            | FileResponse::Seek(..)
            | FileResponse::Write(..)
            | FileResponse::WriteLimited(..)
            | FileResponse::Xstat(..)
            | FileResponse::XstatFs(..)
            | FileResponse::XstatFsV2(..)
            | FileResponse::GetDEnts64(Err(..))
            | FileResponse::Open(Err(..))
            | FileResponse::OpenDir(Err(..))
            | FileResponse::ReadDirBatch(Err(..))
            | FileResponse::ReadLink(..)
            | FileResponse::MakeDir(..)
            | FileResponse::Unlink(..)
            | FileResponse::RemoveDir(..)
            | FileResponse::Rename(..)
            | FileResponse::Ftruncate(..)
            | FileResponse::Futimens(..)
            | FileResponse::Fchown(..)
            | FileResponse::Fchmod(..) => None,

            FileResponse::GetDEnts64(Ok(GetDEnts64Response { fd, .. }))
            | FileResponse::Open(Ok(OpenFileResponse { fd }))
            | FileResponse::OpenDir(Ok(OpenDirResponse { fd, .. }))
            | FileResponse::ReadDirBatch(Ok(ReadDirBatchResponse { fd, .. })) => Some(fd),
        }
    }
}

impl FileResponseExt for DaemonMessage {
    fn remote_fd_mut(&mut self) -> Option<&mut u64> {
        match self {
            Self::File(res) => res.remote_fd_mut(),
            Self::Close(_)
            | Self::Tcp(_)
            | Self::TcpSteal(_)
            | Self::TcpOutgoing(_)
            | Self::UdpOutgoing(_)
            | Self::LogMessage(_)
            | Self::Pong
            | Self::GetEnvVarsResponse(_)
            | Self::GetAddrInfoResponse(_)
            | Self::PauseTarget(_)
            | Self::SwitchProtocolVersionResponse(_)
            | Self::Vpn(_)
            | Self::OperatorPing(_)
            | Self::ReverseDnsLookup(_) => None,
        }
    }
}
