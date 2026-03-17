use std::ops::Not;

use mirrord_protocol::{
    ClientMessage, DaemonMessage, FileRequest, FileResponse, GetEnvVarsRequest, RemoteEnvVars,
    RemoteResult,
    dns::{
        ADDRINFO_V2_VERSION, DnsLookup, GetAddrInfoRequest, GetAddrInfoRequestV2,
        GetAddrInfoResponse, REVERSE_DNS_VERSION, ReverseDnsLookupRequest,
        ReverseDnsLookupResponse,
    },
    file::{
        AccessFileRequest, AccessFileResponse, COPYFILE_VERSION, CloseDirRequest, CloseFileRequest,
        FchmodRequest, FchownRequest, FdOpenDirRequest, FtruncateRequest, FutimensRequest,
        GetDEnts64Request, GetDEnts64Response, MKDIR_VERSION, MakeDirAtRequest, MakeDirRequest,
        OpenDirResponse, OpenFileRequest, OpenFileResponse, OpenRelativeFileRequest,
        READDIR_BATCH_VERSION, READLINK_VERSION, RENAME_VERSION, RMDIR_VERSION,
        ReadDirBatchRequest, ReadDirBatchResponse, ReadDirRequest, ReadDirResponse,
        ReadFileRequest, ReadFileResponse, ReadLimitedFileRequest, ReadLinkFileRequest,
        ReadLinkFileResponse, RemoveDirRequest, RenameRequest, STATFS_V2_VERSION, STATFS_VERSION,
        SeekFileRequest, SeekFileResponse, StatFsRequest, StatFsRequestV2, UnlinkAtRequest,
        UnlinkRequest, WriteFileRequest, WriteFileResponse, WriteLimitedFileRequest,
        XstatFsRequest, XstatFsRequestV2, XstatFsResponse, XstatFsResponseV2, XstatRequest,
        XstatResponse,
    },
};
use tokio::sync::oneshot;

use crate::{
    client::{
        ClientError,
        error::{ClientResult, TaskError, TaskResult},
        queue_kind::QueueKind,
    },
    file_ext::FileRequestExt,
};

/// Trait for simple [`mirrord_protocol`] requests that trigger a response from server, for example
/// [`GetAddrInfoRequest`]s.
pub trait SimpleRequest: Sized {
    /// Type of expected response.
    type Response;

    /// Prepares this request to be processed by the client API, with the given [`mirrord_protocol`]
    /// version.
    ///
    /// This function is expected to handle request downgrade, if necessary.
    /// For example, [`GetAddrInfoRequestV2`] -> [`GetAddrInfoRequest`].
    fn prepare(self, version: &semver::Version) -> ClientResult<Prepared<Self>>;
}

/// Handles a result of a [`SimpleRequest`].
pub trait ResultHandler:
    'static + Send + Sync + FnOnce(ClientResult<DaemonMessage>) -> TaskResult<()>
{
}

impl<F: 'static + Send + Sync + FnOnce(ClientResult<DaemonMessage>) -> TaskResult<()>> ResultHandler
    for F
{
}

/// [`SimpleRequest`] prepared for processing by the client API.
pub struct Prepared<R: SimpleRequest> {
    /// The message that should be sent to the [`mirrord_protocol`] server.
    pub request: ClientMessage,
    /// Response queue in which [`Self::result_handler`] should be store.
    pub queue_kind: QueueKind,
    /// Function to call with the [`mirrord_protocol`] server's response.
    pub result_handler: Box<dyn ResultHandler>,
    /// Channel that will receive the result.
    pub result_rx: oneshot::Receiver<Result<R::Response, ClientError>>,
}

impl SimpleRequest for GetEnvVarsRequest {
    type Response = RemoteEnvVars;

    fn prepare(self, _: &semver::Version) -> ClientResult<Prepared<Self>> {
        let (result_tx, result_rx) = oneshot::channel();
        let result_handler: Box<dyn ResultHandler> =
            Box::new(|result: ClientResult<DaemonMessage>| -> TaskResult<()> {
                let result = match result {
                    Err(error) => Err(error),
                    Ok(DaemonMessage::GetEnvVarsResponse(Ok(vars))) => Ok(vars),
                    Ok(DaemonMessage::GetEnvVarsResponse(Err(error))) => {
                        Err(ClientError::Response(error))
                    }
                    Ok(other) => return Err(TaskError::unexpected_message(&other)),
                };
                result_tx.send(result).ok();
                Ok(())
            });

        Ok(Prepared {
            request: ClientMessage::GetEnvVarsRequest(self),
            queue_kind: QueueKind::EnvVars,
            result_handler,
            result_rx,
        })
    }
}

impl SimpleRequest for GetAddrInfoRequest {
    type Response = DnsLookup;

    fn prepare(self, _: &semver::Version) -> ClientResult<Prepared<Self>> {
        let (result_tx, result_rx) = oneshot::channel();
        let result_handler: Box<dyn ResultHandler> =
            Box::new(|result: ClientResult<DaemonMessage>| -> TaskResult<()> {
                let result = match result {
                    Err(error) => Err(error),
                    Ok(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(Ok(lookup)))) => {
                        Ok(lookup)
                    }
                    Ok(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(Err(error)))) => {
                        Err(ClientError::Response(error))
                    }
                    Ok(other) => return Err(TaskError::unexpected_message(&other)),
                };
                result_tx.send(result).ok();
                Ok(())
            });

        Ok(Prepared {
            request: ClientMessage::GetAddrInfoRequest(self),
            queue_kind: QueueKind::Dns,
            result_handler,
            result_rx,
        })
    }
}

impl SimpleRequest for GetAddrInfoRequestV2 {
    type Response = DnsLookup;

    fn prepare(self, version: &semver::Version) -> ClientResult<Prepared<Self>> {
        let request = if ADDRINFO_V2_VERSION.matches(version) {
            ClientMessage::GetAddrInfoRequestV2(self)
        } else {
            ClientMessage::GetAddrInfoRequest(self.into())
        };

        let (result_tx, result_rx) = oneshot::channel();
        let result_handler: Box<dyn ResultHandler> =
            Box::new(|result: ClientResult<DaemonMessage>| -> TaskResult<()> {
                let result = match result {
                    Err(error) => Err(error),
                    Ok(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(Ok(lookup)))) => {
                        Ok(lookup)
                    }
                    Ok(DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(Err(error)))) => {
                        Err(ClientError::Response(error))
                    }
                    Ok(other) => return Err(TaskError::unexpected_message(&other)),
                };
                result_tx.send(result).ok();
                Ok(())
            });

        Ok(Prepared {
            request,
            queue_kind: QueueKind::Dns,
            result_handler,
            result_rx,
        })
    }
}

impl SimpleRequest for ReverseDnsLookupRequest {
    type Response = String;

    fn prepare(self, version: &semver::Version) -> ClientResult<Prepared<Self>> {
        if REVERSE_DNS_VERSION.matches(version).not() {
            return Err(ClientError::NotSupported);
        }

        let (result_tx, result_rx) = oneshot::channel();
        let result_handler: Box<dyn ResultHandler> =
            Box::new(|result: ClientResult<DaemonMessage>| -> TaskResult<()> {
                let result = match result {
                    Err(error) => Err(error),
                    Ok(DaemonMessage::ReverseDnsLookup(Ok(ReverseDnsLookupResponse {
                        hostname: Ok(hostname),
                    }))) => Ok(hostname),
                    Ok(DaemonMessage::ReverseDnsLookup(Ok(ReverseDnsLookupResponse {
                        hostname: Err(error),
                    }))) => Err(ClientError::Response(error)),
                    Ok(DaemonMessage::ReverseDnsLookup(Err(error))) => {
                        Err(ClientError::Response(error))
                    }
                    Ok(other) => return Err(TaskError::unexpected_message(&other)),
                };
                result_tx.send(result).ok();
                Ok(())
            });

        Ok(Prepared {
            request: ClientMessage::ReverseDnsLookup(self),
            queue_kind: QueueKind::ReverseDns,
            result_handler,
            result_rx,
        })
    }
}

/// Trait for file requests that trigger a response from the server.
pub trait FileRequestWithResponse: FileRequestExt + Sized {
    type Response;

    fn check_and_prepare(self, _: &semver::Version) -> ClientResult<FileRequest> {
        Ok(self.into_file_request())
    }

    fn extract_response(
        general_file_response: FileResponse,
    ) -> Result<RemoteResult<Self::Response>, FileResponse>;
}

impl<T> SimpleRequest for T
where
    T: FileRequestWithResponse,
    T::Response: 'static + Send,
{
    type Response = <Self as FileRequestWithResponse>::Response;

    fn prepare(self, version: &semver::Version) -> ClientResult<Prepared<Self>> {
        let request = self.check_and_prepare(version)?;
        let (result_tx, result_rx) = oneshot::channel();
        let result_handler: Box<dyn ResultHandler> =
            Box::new(|result: ClientResult<DaemonMessage>| -> TaskResult<()> {
                let result = match result {
                    Err(error) => Err(error),
                    Ok(DaemonMessage::File(file_response)) => {
                        match Self::extract_response(file_response) {
                            Ok(result) => result.map_err(From::from),
                            Err(unexpected) => {
                                return Err(TaskError::unexpected_message(&DaemonMessage::File(
                                    unexpected,
                                )));
                            }
                        }
                    }
                    Ok(other) => return Err(TaskError::unexpected_message(&other)),
                };
                result_tx.send(result).ok();
                Ok(())
            });
        Ok(Prepared {
            request: ClientMessage::FileRequest(request),
            queue_kind: QueueKind::Files,
            result_handler,
            result_rx,
        })
    }
}

macro_rules! impl_file_request_with_response {
    ($req:ty, $res:ty, $res_variant:ident) => {
        impl FileRequestWithResponse for $req {
            type Response = $res;

            fn extract_response(
                general_file_response: FileResponse,
            ) -> Result<RemoteResult<Self::Response>, FileResponse> {
                match general_file_response {
                    FileResponse::$res_variant(response) => Ok(response),
                    other => Err(other),
                }
            }
        }
    };

    ($req:ty, $res:ty, $res_variant:ident, $version_req:ident) => {
        impl FileRequestWithResponse for $req {
            type Response = $res;

            fn extract_response(
                general_file_response: FileResponse,
            ) -> Result<RemoteResult<Self::Response>, FileResponse> {
                match general_file_response {
                    FileResponse::$res_variant(response) => Ok(response),
                    other => Err(other),
                }
            }

            fn check_and_prepare(self, version: &semver::Version) -> ClientResult<FileRequest> {
                if $version_req.matches(version) {
                    Ok(self.into_file_request())
                } else {
                    Err(ClientError::NotSupported)
                }
            }
        }
    };
}

impl_file_request_with_response!(OpenFileRequest, OpenFileResponse, Open);
impl_file_request_with_response!(OpenRelativeFileRequest, OpenFileResponse, Open);
impl_file_request_with_response!(ReadFileRequest, ReadFileResponse, Read);
impl_file_request_with_response!(ReadLimitedFileRequest, ReadFileResponse, ReadLimited);
impl_file_request_with_response!(SeekFileRequest, SeekFileResponse, Seek);
impl_file_request_with_response!(WriteFileRequest, WriteFileResponse, Write);
impl_file_request_with_response!(WriteLimitedFileRequest, WriteFileResponse, WriteLimited);
impl_file_request_with_response!(AccessFileRequest, AccessFileResponse, Access);
impl_file_request_with_response!(XstatRequest, XstatResponse, Xstat);
impl_file_request_with_response!(XstatFsRequest, XstatFsResponse, XstatFs);
impl_file_request_with_response!(FdOpenDirRequest, OpenDirResponse, OpenDir);
impl_file_request_with_response!(ReadDirRequest, ReadDirResponse, ReadDir);
impl_file_request_with_response!(GetDEnts64Request, GetDEnts64Response, GetDEnts64);
impl_file_request_with_response!(
    ReadLinkFileRequest,
    ReadLinkFileResponse,
    ReadLink,
    READLINK_VERSION
);
impl_file_request_with_response!(
    ReadDirBatchRequest,
    ReadDirBatchResponse,
    ReadDirBatch,
    READDIR_BATCH_VERSION
);
impl_file_request_with_response!(MakeDirRequest, (), MakeDir, MKDIR_VERSION);
impl_file_request_with_response!(MakeDirAtRequest, (), MakeDir, MKDIR_VERSION);
impl_file_request_with_response!(RemoveDirRequest, (), RemoveDir, RMDIR_VERSION);
impl_file_request_with_response!(UnlinkRequest, (), Unlink, RMDIR_VERSION);
impl_file_request_with_response!(UnlinkAtRequest, (), Unlink, RMDIR_VERSION);
impl_file_request_with_response!(StatFsRequest, XstatFsResponse, XstatFs, STATFS_VERSION);
impl_file_request_with_response!(RenameRequest, (), Rename, RENAME_VERSION);
impl_file_request_with_response!(FtruncateRequest, (), Ftruncate, COPYFILE_VERSION);
impl_file_request_with_response!(FutimensRequest, (), Futimens, COPYFILE_VERSION);
impl_file_request_with_response!(FchownRequest, (), Fchown, COPYFILE_VERSION);
impl_file_request_with_response!(FchmodRequest, (), Fchmod, COPYFILE_VERSION);

impl FileRequestWithResponse for XstatFsRequestV2 {
    type Response = XstatFsResponseV2;

    fn check_and_prepare(self, version: &semver::Version) -> ClientResult<FileRequest> {
        if STATFS_V2_VERSION.matches(version) {
            Ok(FileRequest::XstatFsV2(self))
        } else {
            Ok(FileRequest::XstatFs(self.into()))
        }
    }

    fn extract_response(
        general_file_response: FileResponse,
    ) -> Result<RemoteResult<Self::Response>, FileResponse> {
        match general_file_response {
            FileResponse::XstatFsV2(result) => Ok(result),
            FileResponse::XstatFs(result) => Ok(result.map(From::from)),
            other => Err(other),
        }
    }
}

impl FileRequestWithResponse for StatFsRequestV2 {
    type Response = XstatFsResponseV2;

    fn check_and_prepare(self, version: &semver::Version) -> ClientResult<FileRequest> {
        if STATFS_V2_VERSION.matches(version) {
            Ok(FileRequest::StatFsV2(self))
        } else if STATFS_VERSION.matches(version) {
            Ok(FileRequest::StatFs(self.into()))
        } else {
            Err(ClientError::NotSupported)
        }
    }

    fn extract_response(
        general_file_response: FileResponse,
    ) -> Result<RemoteResult<Self::Response>, FileResponse> {
        match general_file_response {
            FileResponse::XstatFsV2(result) => Ok(result),
            FileResponse::XstatFs(result) => Ok(result.map(From::from)),
            other => Err(other),
        }
    }
}

/// Trait for simple [`mirrord_protocol`] requests that do not trigger any response from server.
///
/// Currently only [`CloseFileRequest`] and [`CloseDirRequest`].
pub trait SimpleRequestNoResponse {
    /// Wraps this request into a [`ClientMessage`] to be sent to the server.
    fn prepare(self) -> ClientMessage;
}

impl SimpleRequestNoResponse for CloseFileRequest {
    fn prepare(self) -> ClientMessage {
        ClientMessage::FileRequest(FileRequest::Close(self))
    }
}

impl SimpleRequestNoResponse for CloseDirRequest {
    fn prepare(self) -> ClientMessage {
        ClientMessage::FileRequest(FileRequest::CloseDir(self))
    }
}
