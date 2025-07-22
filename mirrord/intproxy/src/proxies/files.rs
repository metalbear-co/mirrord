use core::fmt;
use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet, VecDeque},
    vec,
};

use mirrord_intproxy_protocol::{LayerId, MessageId, ProxyToLayerMessage};
use mirrord_protocol::{
    file::*, ClientMessage, DaemonMessage, ErrorKindInternal, FileRequest, FileResponse,
    RemoteIOError, ResponseError,
};
use semver::Version;
use thiserror::Error;
use tracing::Level;

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    error::{agent_lost_io_error, UnexpectedAgentMessage},
    main_tasks::{LayerClosed, LayerForked, ProxyMessage, ToLayer},
    remote_resources::RemoteResources,
    request_queue::RequestQueue,
};

macro_rules! dummy_file_response {
    ($name: ident) => {
        FileResponse::$name(Err(ResponseError::NotImplemented))
    };
}

/// Lightweight (no allocations) [`FileResponse`] to be returned when connection with the
/// mirrord-agent is lost. Must be converted into real [`FileResponse`] via [`From`].
pub struct AgentLostFileResponse(LayerId, MessageId, FileResponse);

impl From<AgentLostFileResponse> for ToLayer {
    fn from(value: AgentLostFileResponse) -> Self {
        let AgentLostFileResponse(layer_id, message_id, response) = value;
        let error = agent_lost_io_error();

        let real_response = match response {
            FileResponse::Access(..) => FileResponse::Access(Err(error)),
            FileResponse::GetDEnts64(..) => FileResponse::GetDEnts64(Err(error)),
            FileResponse::Open(..) => FileResponse::Open(Err(error)),
            FileResponse::OpenDir(..) => FileResponse::OpenDir(Err(error)),
            FileResponse::Read(..) => FileResponse::Read(Err(error)),
            FileResponse::ReadDir(..) => FileResponse::ReadDir(Err(error)),
            FileResponse::ReadDirBatch(..) => FileResponse::ReadDirBatch(Err(error)),
            FileResponse::ReadLimited(..) => FileResponse::ReadLimited(Err(error)),
            FileResponse::Seek(..) => FileResponse::Seek(Err(error)),
            FileResponse::Write(..) => FileResponse::Write(Err(error)),
            FileResponse::WriteLimited(..) => FileResponse::WriteLimited(Err(error)),
            FileResponse::Xstat(..) => FileResponse::Xstat(Err(error)),
            FileResponse::XstatFs(..) => FileResponse::XstatFs(Err(error)),
            FileResponse::XstatFsV2(..) => FileResponse::XstatFsV2(Err(error)),
            FileResponse::ReadLink(..) => FileResponse::ReadLink(Err(error)),
            FileResponse::MakeDir(..) => FileResponse::MakeDir(Err(error)),
            FileResponse::RemoveDir(..) => FileResponse::RemoveDir(Err(error)),
            FileResponse::Unlink(..) => FileResponse::Unlink(Err(error)),
        };

        debug_assert_eq!(
            std::mem::discriminant(&response),
            std::mem::discriminant(&real_response),
        );

        ToLayer {
            layer_id,
            message_id,
            message: ProxyToLayerMessage::File(real_response),
        }
    }
}

/// Convenience trait for [`FileRequest`].
trait FileRequestExt: Sized {
    /// If this [`FileRequest`] requires a [`FileResponse`] from the agent, return corresponding
    /// [`AgentLostFileResponse`].
    fn agent_lost_response(
        &self,
        layer_id: LayerId,
        message_id: MessageId,
    ) -> Option<AgentLostFileResponse>;
}

impl FileRequestExt for FileRequest {
    fn agent_lost_response(
        &self,
        layer_id: LayerId,
        message_id: MessageId,
    ) -> Option<AgentLostFileResponse> {
        let response = match self {
            Self::Close(..) | Self::CloseDir(..) => return None,
            Self::Access(..) => dummy_file_response!(Access),
            Self::FdOpenDir(..) => dummy_file_response!(OpenDir),
            Self::GetDEnts64(..) => dummy_file_response!(GetDEnts64),
            Self::Open(..) => dummy_file_response!(Open),
            Self::OpenRelative(..) => dummy_file_response!(Open),
            Self::Read(..) => dummy_file_response!(Read),
            Self::ReadDir(..) => dummy_file_response!(ReadDir),
            Self::ReadDirBatch(..) => dummy_file_response!(ReadDirBatch),
            Self::ReadLimited(..) => dummy_file_response!(ReadLimited),
            Self::Seek(..) => dummy_file_response!(Seek),
            Self::Write(..) => dummy_file_response!(Write),
            Self::WriteLimited(..) => dummy_file_response!(WriteLimited),
            Self::Xstat(..) => dummy_file_response!(Xstat),
            Self::XstatFs(..) => dummy_file_response!(XstatFs),
            Self::XstatFsV2(..) => dummy_file_response!(XstatFsV2),
            Self::ReadLink(..) => dummy_file_response!(ReadLink),
            Self::MakeDir(..) => dummy_file_response!(MakeDir),
            Self::MakeDirAt(..) => dummy_file_response!(MakeDir),
            Self::Unlink(..) => dummy_file_response!(Unlink),
            Self::UnlinkAt(..) => dummy_file_response!(Unlink),
            Self::RemoveDir(..) => dummy_file_response!(RemoveDir),
            Self::StatFs(..) => dummy_file_response!(XstatFs),
            Self::StatFsV2(..) => dummy_file_response!(XstatFsV2),
        };

        Some(AgentLostFileResponse(layer_id, message_id, response))
    }
}

/// Messages handled by [`FilesProxy`].
#[derive(Debug)]
pub enum FilesProxyMessage {
    /// Layer sent file request.
    FileReq(MessageId, LayerId, FileRequest),
    /// Agent sent file response.
    FileRes(FileResponse),
    /// Protocol version was negotiated with the agent.
    ProtocolVersion(Version),
    /// Layer instance forked.
    LayerForked(LayerForked),
    /// Layer instance closed.
    LayerClosed(LayerClosed),
    /// Agent connection was refreshed
    ConnectionRefresh,
}

/// Error that can occur in [`FilesProxy`].
#[derive(Error, Debug)]
#[error(transparent)]
pub struct FilesProxyError(#[from] UnexpectedAgentMessage);

/// Locally cached data of a remote file that is buffered.
#[derive(Default)]
struct BufferedFileData {
    /// Buffered file contents.
    buffer: Vec<u8>,
    /// Position of [`Self::buffer`] in the file.
    buffer_position: u64,
    /// Position of the file descriptor in the file.
    /// This position is normally managed in the agent,
    /// but for buffered files we manage it here.
    /// It's simpler this way.
    fd_position: u64,
}

impl BufferedFileData {
    /// Attempts to read `amount` bytes from [`Self::buffer`], starting from `position` in the file.
    ///
    /// Returns [`None`] when the read does not fit in the buffer in whole.
    fn read_from_buffer(&self, amount: u64, position: u64) -> Option<&[u8]> {
        let start_from = position.checked_sub(self.buffer_position)? as usize;
        let end_before = start_from + amount as usize;
        self.buffer.get(start_from..end_before)
    }
}

impl fmt::Debug for BufferedFileData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufferedFileData")
            .field("buffer_position", &self.buffer_position)
            .field("buffer_len", &self.buffer.len())
            .field("fd_position", &self.fd_position)
            .finish()
    }
}

/// Locally cached data of a remote directory that is buffered.
#[derive(Default)]
struct BufferedDirData {
    /// Buffered entries of this directory.
    buffered_entries: vec::IntoIter<DirEntryInternal>,
}

impl fmt::Debug for BufferedDirData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufferedDirData")
            .field("remaining_buffered_entries", &self.buffered_entries.len())
            .finish()
    }
}

/// Additional request data that is saved by [`FilesProxy`] in its [`RequestQueue`].
/// Allows for handling buffered reads by marking requests that should be handled in a special way.
#[derive(Debug, Default)]
enum AdditionalRequestData {
    /// Open file that will be buffered.
    OpenBuffered,

    /// Read file that is buffered.
    ReadBuffered {
        /// File descriptor.
        fd: u64,
        /// Read buffer size of the user application.
        /// The user requested reading this many bytes.
        requested_amount: u64,
        /// Whether we should update fd position in file
        /// (we store it locally).
        update_fd_position: bool,
    },

    /// Seek file that is buffered.
    SeekBuffered {
        /// File descriptor.
        fd: u64,
    },

    /// All other file ops.
    #[default]
    Other,
}

/// Manages state of file operations. Remaps remote file descriptors and returns early
/// [`ResponseError`]s for [`FileRequest`]s related to invalidated (agent lost) descriptors.
/// Tracks state of outstanding [`FileRequest`]s to respond with errors in case the agent is lost.
#[derive(Default)]
pub struct RouterFileOps {
    /// Highest file fd we've returned to the client (after remapping).
    highest_user_facing_fd: Option<u64>,
    /// Offset we need to add to every fd we receive from the mirrord-agent.
    /// All lesser fds received from the clients are invalid (probably lost with previous
    /// mirrord-agent responsible for file ops).
    current_fd_offset: u64,
    /// Prepared error responses to outstanding [`FileRequest`]s.
    /// We must flush these when connection to the mirrord-agent is lost, otherwise the layer will
    /// hang.
    queued_error_responses: VecDeque<AgentLostFileResponse>,
}

impl fmt::Debug for RouterFileOps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RouterFileOps")
            .field("highest_user_facing_fd", &self.highest_user_facing_fd)
            .field("current_fd_offset", &self.current_fd_offset)
            .finish()
    }
}

impl RouterFileOps {
    /// Return a request to be sent to the agent ([`Ok`] variant) or
    /// a response to be sent to the user ([`Err`] variant).
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE, Debug))]
    pub fn map_request(
        &mut self,
        layer_id: LayerId,
        message_id: MessageId,
        mut request: FileRequest,
    ) -> Result<Option<FileRequest>, ToLayer> {
        match &mut request {
            // These requests do not refer to any open remote fd.
            // It's safe to pass them as they are.
            FileRequest::Open(..)
            | FileRequest::Access(..)
            | FileRequest::Xstat(XstatRequest { fd: None, .. })
            | FileRequest::ReadLink(..)
            | FileRequest::MakeDir(..)
            | FileRequest::Unlink(..)
            | FileRequest::RemoveDir(..)
            | FileRequest::StatFs(..)
            | FileRequest::StatFsV2(..)
            | FileRequest::UnlinkAt(UnlinkAtRequest { dirfd: None, .. }) => {}

            // These requests do not require any response from the agent.
            // We need to remap the fd, but if the fd is invalid we simply drop them.
            FileRequest::Close(CloseFileRequest { fd: remote_fd })
            | FileRequest::CloseDir(CloseDirRequest { remote_fd }) => {
                if *remote_fd < self.current_fd_offset {
                    return Ok(None);
                }

                *remote_fd -= self.current_fd_offset;
            }

            // These requests refer to an open remote fd and require a response from the agent.
            // We need to remap the fd and respond with an error if the fd is invalid.
            FileRequest::FdOpenDir(FdOpenDirRequest { remote_fd })
            | FileRequest::GetDEnts64(GetDEnts64Request { remote_fd, .. })
            | FileRequest::OpenRelative(OpenRelativeFileRequest {
                relative_fd: remote_fd,
                ..
            })
            | FileRequest::Read(ReadFileRequest { remote_fd, .. })
            | FileRequest::ReadDir(ReadDirRequest { remote_fd, .. })
            | FileRequest::ReadDirBatch(ReadDirBatchRequest { remote_fd, .. })
            | FileRequest::ReadLimited(ReadLimitedFileRequest { remote_fd, .. })
            | FileRequest::Seek(SeekFileRequest { fd: remote_fd, .. })
            | FileRequest::Write(WriteFileRequest { fd: remote_fd, .. })
            | FileRequest::WriteLimited(WriteLimitedFileRequest { remote_fd, .. })
            | FileRequest::Xstat(XstatRequest {
                fd: Some(remote_fd),
                ..
            })
            | FileRequest::XstatFs(XstatFsRequest { fd: remote_fd })
            | FileRequest::XstatFsV2(XstatFsRequestV2 { fd: remote_fd })
            | FileRequest::MakeDirAt(MakeDirAtRequest {
                dirfd: remote_fd, ..
            })
            | FileRequest::UnlinkAt(UnlinkAtRequest {
                dirfd: Some(remote_fd),
                ..
            }) => {
                if *remote_fd < self.current_fd_offset {
                    let error_response = request
                        .agent_lost_response(layer_id, message_id)
                        .expect("these requests require responses")
                        .into();
                    return Err(error_response);
                }

                *remote_fd -= self.current_fd_offset;
            }
        };

        if let Some(response) = request.agent_lost_response(layer_id, message_id) {
            self.queued_error_responses.push_back(response);
        }

        Ok(Some(request))
    }

    /// Return a response to be sent to the client.
    #[tracing::instrument(level = Level::TRACE, ret)]
    pub fn map_response(&mut self, mut response: FileResponse) -> FileResponse {
        match &mut response {
            // These responses do not refer to any open remote fd.
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
            | FileResponse::RemoveDir(..) => {}

            FileResponse::GetDEnts64(Ok(GetDEnts64Response { fd: remote_fd, .. }))
            | FileResponse::Open(Ok(OpenFileResponse { fd: remote_fd }))
            | FileResponse::OpenDir(Ok(OpenDirResponse { fd: remote_fd, .. }))
            | FileResponse::ReadDirBatch(Ok(ReadDirBatchResponse { fd: remote_fd, .. })) => {
                *remote_fd += self.current_fd_offset;

                self.highest_user_facing_fd =
                    std::cmp::max(self.highest_user_facing_fd, Some(*remote_fd));
            }
        }

        self.queued_error_responses.pop_front();

        response
    }

    /// Notify this manager that the agent was lost.
    /// Return messages to be sent to the user.
    #[tracing::instrument(level = Level::TRACE)]
    pub fn agent_lost(&mut self) -> VecDeque<AgentLostFileResponse> {
        self.current_fd_offset = self.highest_user_facing_fd.map(|fd| fd + 1).unwrap_or(0);
        std::mem::take(&mut self.queued_error_responses)
    }
}

/// For handling all file operations.
/// Run as a [`BackgroundTask`].
///
/// # Directory buffering
///
/// To optimize cases where user application traverses large directories,
/// we use [`FileRequest::ReadDirBatch`] to fetch many entries at once
/// ([`Self::READDIR_BATCH_SIZE`]).
///
/// Excessive entries are cached locally in this proxy and used until depleted.
///
/// # File buffering
///
/// To optimize cases where user application makes a lot of small reads on remote files,
/// we change the way of reading readonly files.
///
/// 1. When created with [`FilesProxy::new`], this proxy is given a desired file buffer size. Buffer
///    size 0 disables file buffering.
/// 2. When the user requests a read, we fetch at least `buffer_size` bytes. We return the amount
///    requested by the user and store the whole response as a local buffer.
/// 3. When the user requests a read again, we try to fulfill the request using only the local
///    buffer. If it's not possible, we proceed as in point 1
/// 4. To solve problems with descriptor offset, we only use [`FileRequest::ReadLimited`] to read
///    buffered files. Descriptor offset value is maintained in this proxy.
pub struct FilesProxy {
    /// [`mirrord_protocol`] version negotiated with the agent.
    /// Determines whether we can use some messages, like [`FileRequest::ReadDirBatch`] or
    /// [`FileRequest::ReadLink`].
    protocol_version: Option<Version>,

    /// Size for readonly files buffer.
    /// If equal to 0, this proxy does not buffer files.
    file_buffer_size: u64,

    /// Stores metadata of outstanding requests.
    request_queue: RequestQueue<AdditionalRequestData>,

    /// For tracking remote file descriptors across layer instances (forks).
    remote_files: RemoteResources<u64>,
    /// Locally stored data of buffered files.
    buffered_files: HashMap<u64, BufferedFileData>,

    /// For tracking remote directory descriptors across layer instances (forks).
    remote_dirs: RemoteResources<u64>,
    /// Locally stored data of buffered directories.
    buffered_dirs: HashMap<u64, BufferedDirData>,

    reconnect_tracker: RouterFileOps,
}

impl fmt::Debug for FilesProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesProxy")
            .field("file_buffer_size", &self.file_buffer_size)
            .field("buffer_readdir", &self.buffer_dirs())
            .field("buffered_files", &self.buffered_files)
            .field("buffered_dirs", &self.buffered_dirs)
            .field("protocol_version", &self.protocol_version)
            .field("request_queue", &self.request_queue)
            .field("reconnect_tracker", &self.reconnect_tracker)
            .finish()
    }
}

impl FilesProxy {
    /// How many directory entries we request at a time.
    /// Relevant only if [`mirrord_protocol`] version allows for [`FileRequest::ReadDirBatch`].
    pub const READDIR_BATCH_SIZE: usize = 128;

    /// Creates a new files proxy instance.
    /// Proxy can be used as a [`BackgroundTask`].
    ///
    /// `file_buffer_size` sets size of the readonly files buffer.
    /// Size 0 disables buffering.
    pub fn new(file_buffer_size: u64) -> Self {
        Self {
            protocol_version: Default::default(),
            file_buffer_size,

            request_queue: Default::default(),

            remote_files: Default::default(),
            buffered_files: Default::default(),

            remote_dirs: Default::default(),
            buffered_dirs: Default::default(),

            reconnect_tracker: Default::default(),
        }
    }

    /// Returns whether [`mirrord_protocol`] version allows for buffering directories.
    fn buffer_dirs(&self) -> bool {
        self.protocol_version
            .as_ref()
            .is_some_and(|version| READDIR_BATCH_VERSION.matches(version))
    }

    /// Returns whether this proxy is configured to buffer readonly files.
    fn buffer_reads(&self) -> bool {
        self.file_buffer_size > 0
    }

    #[tracing::instrument(level = Level::TRACE)]
    fn layer_forked(&mut self, forked: LayerForked) {
        self.remote_files.clone_all(forked.parent, forked.child);
        self.remote_dirs.clone_all(forked.parent, forked.child);
    }

    #[tracing::instrument(level = Level::TRACE, skip(message_bus))]
    async fn layer_closed(&mut self, closed: LayerClosed, message_bus: &mut MessageBus<Self>) {
        for fd in self.remote_files.remove_all(closed.id) {
            self.buffered_files.remove(&fd);
            message_bus
                .send(ProxyMessage::ToAgent(ClientMessage::FileRequest(
                    FileRequest::Close(CloseFileRequest { fd }),
                )))
                .await;
        }

        for remote_fd in self.remote_dirs.remove_all(closed.id) {
            self.buffered_dirs.remove(&remote_fd);
            message_bus
                .send(ProxyMessage::ToAgent(ClientMessage::FileRequest(
                    FileRequest::CloseDir(CloseDirRequest { remote_fd }),
                )))
                .await;
        }
    }

    #[tracing::instrument(level = Level::TRACE)]
    fn protocol_version(&mut self, version: Version) {
        self.protocol_version.replace(version);
    }

    /// Checks if the mirrord protocol version supports this [`FileRequest`].
    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err(level = Level::WARN, Debug))]
    fn is_request_supported(&self, request: &FileRequest) -> Result<(), FileResponse> {
        let protocol_version = self.protocol_version.as_ref();

        match request {
            FileRequest::ReadLink(..)
                if protocol_version.is_none_or(|version| !READLINK_VERSION.matches(version)) =>
            {
                Err(FileResponse::ReadLink(Err(ResponseError::NotImplemented)))
            }
            FileRequest::MakeDir(..) | FileRequest::MakeDirAt(..)
                if protocol_version.is_none_or(|version| !MKDIR_VERSION.matches(version)) =>
            {
                Err(FileResponse::MakeDir(Err(ResponseError::NotImplemented)))
            }
            FileRequest::RemoveDir(..) | FileRequest::Unlink(..) | FileRequest::UnlinkAt(..)
                if protocol_version
                    .is_none_or(|version: &Version| !RMDIR_VERSION.matches(version)) =>
            {
                Err(FileResponse::RemoveDir(Err(ResponseError::NotImplemented)))
            }
            FileRequest::StatFs(..) | FileRequest::StatFsV2(..)
                if protocol_version
                    .is_none_or(|version: &Version| !STATFS_VERSION.matches(version)) =>
            {
                Err(FileResponse::XstatFs(Err(ResponseError::NotImplemented)))
            }
            _ => Ok(()),
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(message_bus), ret)]
    async fn file_request(
        &mut self,
        request: FileRequest,
        layer_id: LayerId,
        message_id: MessageId,
        message_bus: &mut MessageBus<Self>,
    ) {
        // Not supported in old `mirrord-protocol` versions.
        if let Err(response) = self.is_request_supported(&request) {
            message_bus
                .send(ToLayer {
                    message_id,
                    layer_id,
                    message: ProxyToLayerMessage::File(response),
                })
                .await;
            return;
        }

        match request {
            // Should trigger remote close only when the fd is closed in all layer instances.
            FileRequest::Close(close) => {
                if self.remote_files.remove(layer_id, close.fd) {
                    self.buffered_files.remove(&close.fd);
                    message_bus
                        .send(ClientMessage::FileRequest(FileRequest::Close(close)))
                        .await;
                }
            }

            // Should trigger remote close only when the fd is closed in all layer instances.
            FileRequest::CloseDir(close) => {
                if self.remote_dirs.remove(layer_id, close.remote_fd) {
                    self.buffered_dirs.remove(&close.remote_fd);
                    message_bus
                        .send(ClientMessage::FileRequest(FileRequest::CloseDir(close)))
                        .await;
                }
            }

            // May require storing additional data in the request queue.
            FileRequest::Open(open) => {
                let additional_data = (self.buffer_reads() && open.open_options.is_read_only())
                    .then_some(AdditionalRequestData::OpenBuffered)
                    .unwrap_or_default();
                self.request_queue
                    .push_back_with_data(message_id, layer_id, additional_data);
                message_bus
                    .send(ClientMessage::FileRequest(FileRequest::Open(open)))
                    .await;
            }

            // May require storing additional data in the request queue.
            FileRequest::OpenRelative(open) => {
                let additional_data = (self.buffer_reads() && open.open_options.is_read_only())
                    .then_some(AdditionalRequestData::OpenBuffered)
                    .unwrap_or_default();
                self.request_queue
                    .push_back_with_data(message_id, layer_id, additional_data);
                message_bus
                    .send(ClientMessage::FileRequest(FileRequest::OpenRelative(open)))
                    .await;
            }

            // Try to use local buffer if possible.
            FileRequest::Read(read) => match self.buffered_files.get_mut(&read.remote_fd) {
                // File is buffered.
                Some(data) => {
                    let from_buffer = data.read_from_buffer(read.buffer_size, data.fd_position);
                    if let Some(from_buffer) = from_buffer {
                        let bytes = from_buffer.to_vec();
                        data.fd_position += read.buffer_size;
                        message_bus
                            .send(ToLayer {
                                message_id,
                                layer_id,
                                message: ProxyToLayerMessage::File(FileResponse::Read(Ok(
                                    ReadFileResponse {
                                        bytes: bytes.into(),
                                        read_amount: read.buffer_size,
                                    },
                                ))),
                            })
                            .await;
                    } else {
                        let additional_data = AdditionalRequestData::ReadBuffered {
                            fd: read.remote_fd,
                            requested_amount: read.buffer_size,
                            update_fd_position: true,
                        };
                        self.request_queue.push_back_with_data(
                            message_id,
                            layer_id,
                            additional_data,
                        );
                        message_bus
                            .send(ClientMessage::FileRequest(FileRequest::ReadLimited(
                                ReadLimitedFileRequest {
                                    remote_fd: read.remote_fd,
                                    buffer_size: std::cmp::max(
                                        read.buffer_size,
                                        self.file_buffer_size,
                                    ),
                                    start_from: data.fd_position,
                                },
                            )))
                            .await;
                    }
                }

                // File is not buffered.
                None => {
                    self.request_queue.push_back(message_id, layer_id);
                    message_bus
                        .send(ClientMessage::FileRequest(FileRequest::Read(read)))
                        .await;
                }
            },

            // Try to use local buffer if possible.
            FileRequest::ReadLimited(read) => match self.buffered_files.get_mut(&read.remote_fd) {
                // File is buffered.
                Some(data) => {
                    let from_buffer = data.read_from_buffer(read.buffer_size, read.start_from);
                    if let Some(from_buffer) = from_buffer {
                        let bytes = from_buffer.to_vec();
                        message_bus
                            .send(ToLayer {
                                message_id,
                                layer_id,
                                message: ProxyToLayerMessage::File(FileResponse::ReadLimited(Ok(
                                    ReadFileResponse {
                                        bytes: bytes.into(),
                                        read_amount: read.buffer_size,
                                    },
                                ))),
                            })
                            .await;
                    } else {
                        let additional_data = AdditionalRequestData::ReadBuffered {
                            fd: read.remote_fd,
                            requested_amount: read.buffer_size,
                            update_fd_position: false,
                        };
                        self.request_queue.push_back_with_data(
                            message_id,
                            layer_id,
                            additional_data,
                        );
                        message_bus
                            .send(ClientMessage::FileRequest(FileRequest::ReadLimited(
                                ReadLimitedFileRequest {
                                    remote_fd: read.remote_fd,
                                    buffer_size: std::cmp::max(
                                        read.buffer_size,
                                        self.file_buffer_size,
                                    ),
                                    start_from: read.start_from,
                                },
                            )))
                            .await;
                    }
                }

                // File is not buffered.
                None => {
                    self.request_queue.push_back(message_id, layer_id);
                    message_bus
                        .send(ClientMessage::FileRequest(FileRequest::ReadLimited(read)))
                        .await;
                }
            },

            // Try to use local buffer if possible.
            FileRequest::ReadDir(read_dir) => match self.buffered_dirs.get_mut(&read_dir.remote_fd)
            {
                // Directory is buffered.
                Some(data) => {
                    if let Some(direntry) = data.buffered_entries.next() {
                        message_bus
                            .send(ToLayer {
                                message_id,
                                layer_id,
                                message: ProxyToLayerMessage::File(FileResponse::ReadDir(Ok(
                                    ReadDirResponse {
                                        direntry: Some(direntry),
                                    },
                                ))),
                            })
                            .await;
                    } else {
                        self.request_queue.push_back(message_id, layer_id);
                        message_bus
                            .send(ClientMessage::FileRequest(FileRequest::ReadDirBatch(
                                ReadDirBatchRequest {
                                    remote_fd: read_dir.remote_fd,
                                    amount: Self::READDIR_BATCH_SIZE,
                                },
                            )))
                            .await;
                    }
                }

                // Directory is not buffered.
                None => {
                    self.request_queue.push_back(message_id, layer_id);
                    message_bus
                        .send(ClientMessage::FileRequest(FileRequest::ReadDir(read_dir)))
                        .await;
                }
            },

            // Should only be sent from intproxy, not from the layer.
            FileRequest::ReadDirBatch(..) => {
                unreachable!("ReadDirBatch request is never sent from the layer");
            }

            // May require storing additional data in the request queue.
            FileRequest::Seek(mut seek) => {
                let additional_data =
                    match (self.buffered_files.get_mut(&seek.fd), &mut seek.seek_from) {
                        (Some(data), SeekFromInternal::Current(diff)) => {
                            let result = u64::try_from(data.fd_position as i128 + *diff as i128);
                            match result {
                                Ok(offset) => seek.seek_from = SeekFromInternal::Start(offset),
                                Err(..) => {
                                    message_bus
                                        .send(ToLayer {
                                            message_id,
                                            layer_id,
                                            message: ProxyToLayerMessage::File(FileResponse::Seek(
                                                Err(ResponseError::RemoteIO(RemoteIOError {
                                                    raw_os_error: Some(22), // EINVAL
                                                    kind: ErrorKindInternal::InvalidInput,
                                                })),
                                            )),
                                        })
                                        .await;
                                    return;
                                }
                            }

                            AdditionalRequestData::SeekBuffered { fd: seek.fd }
                        }
                        (Some(..), _) => AdditionalRequestData::SeekBuffered { fd: seek.fd },
                        _ => AdditionalRequestData::Other,
                    };

                self.request_queue
                    .push_back_with_data(message_id, layer_id, additional_data);
                message_bus
                    .send(ClientMessage::FileRequest(FileRequest::Seek(seek)))
                    .await;
            }
            FileRequest::StatFsV2(statfs_v2)
                if self
                    .protocol_version
                    .as_ref()
                    .is_none_or(|version| !STATFS_V2_VERSION.matches(version)) =>
            {
                self.request_queue.push_back(message_id, layer_id);
                message_bus
                    .send(ClientMessage::FileRequest(FileRequest::StatFs(
                        statfs_v2.into(),
                    )))
                    .await;
            }
            FileRequest::XstatFsV2(xstatfs_v2)
                if self
                    .protocol_version
                    .as_ref()
                    .is_none_or(|version| !STATFS_V2_VERSION.matches(version)) =>
            {
                self.request_queue.push_back(message_id, layer_id);
                message_bus
                    .send(ClientMessage::FileRequest(FileRequest::XstatFs(
                        xstatfs_v2.into(),
                    )))
                    .await;
            }

            // Doesn't require any special logic.
            other => {
                self.request_queue.push_back(message_id, layer_id);
                message_bus.send(ClientMessage::FileRequest(other)).await;
            }
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(message_bus), ret, err)]
    async fn file_response(
        &mut self,
        response: FileResponse,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), FilesProxyError> {
        match response {
            // Update file maps.
            FileResponse::Open(Ok(open)) => {
                let (message_id, layer_id, additional_data) =
                    self.request_queue.pop_front_with_data().ok_or_else(|| {
                        UnexpectedAgentMessage(DaemonMessage::File(FileResponse::Open(Ok(
                            open.clone()
                        ))))
                    })?;

                self.remote_files.add(layer_id, open.fd);

                if matches!(additional_data, AdditionalRequestData::OpenBuffered) {
                    self.buffered_files.insert(open.fd, Default::default());
                }

                message_bus
                    .send(ToLayer {
                        layer_id,
                        message_id,
                        message: ProxyToLayerMessage::File(FileResponse::Open(Ok(open))),
                    })
                    .await;
            }

            // Update dir maps.
            FileResponse::OpenDir(Ok(open)) => {
                let (message_id, layer_id) = self.request_queue.pop_front().ok_or_else(|| {
                    UnexpectedAgentMessage(DaemonMessage::File(FileResponse::OpenDir(Ok(
                        open.clone()
                    ))))
                })?;

                self.remote_dirs.add(layer_id, open.fd);

                if self.buffer_dirs() {
                    self.buffered_dirs.insert(open.fd, Default::default());
                }

                message_bus
                    .send(ToLayer {
                        layer_id,
                        message_id,
                        message: ProxyToLayerMessage::File(FileResponse::OpenDir(Ok(open))),
                    })
                    .await;
            }

            // If the file is buffered, update `files_data`.
            FileResponse::ReadLimited(Ok(read)) => {
                let (message_id, layer_id, additional_data) =
                    self.request_queue.pop_front_with_data().ok_or_else(|| {
                        UnexpectedAgentMessage(DaemonMessage::File(FileResponse::ReadLimited(Ok(
                            read.clone(),
                        ))))
                    })?;

                let AdditionalRequestData::ReadBuffered {
                    fd,
                    requested_amount,
                    update_fd_position,
                } = additional_data
                else {
                    // This file is not buffered.
                    message_bus
                        .send(ToLayer {
                            message_id,
                            layer_id,
                            message: ProxyToLayerMessage::File(FileResponse::ReadLimited(Ok(read))),
                        })
                        .await;
                    return Ok(());
                };

                let Some(data) = self.buffered_files.get_mut(&fd) else {
                    // File must have been closed from other thread in user application.
                    message_bus
                        .send(ToLayer {
                            message_id,
                            layer_id,
                            message: ProxyToLayerMessage::File(FileResponse::ReadLimited(Err(
                                ResponseError::NotFound(fd),
                            ))),
                        })
                        .await;
                    return Ok(());
                };

                let bytes = read
                    .bytes
                    .get(..requested_amount as usize)
                    .unwrap_or(read.bytes.borrow())
                    .to_vec();
                let read_amount = bytes.len() as u64;
                let response = ReadFileResponse {
                    bytes: bytes.into(),
                    read_amount,
                };

                data.buffer = read.bytes.into_vec();
                data.buffer_position = data.fd_position;
                let message = if update_fd_position {
                    // User originally sent `FileRequest::Read`.
                    data.fd_position += response.read_amount;
                    FileResponse::Read(Ok(response))
                } else {
                    // User originally sent `FileRequest::ReadLimited`.
                    FileResponse::ReadLimited(Ok(response))
                };

                message_bus
                    .send(ToLayer {
                        message_id,
                        layer_id,
                        message: ProxyToLayerMessage::File(message),
                    })
                    .await;
            }

            FileResponse::ReadLimited(Err(error)) => {
                // need to ensure that if a Read request was sent by layer, a Read response is
                // returned containing the error rather than a ReadLimited
                let (message_id, layer_id, additional_data) =
                    self.request_queue.pop_front_with_data().ok_or_else(|| {
                        UnexpectedAgentMessage(DaemonMessage::File(FileResponse::ReadLimited(Err(
                            error.clone(),
                        ))))
                    })?;

                let message = match additional_data {
                    AdditionalRequestData::ReadBuffered {
                        update_fd_position, ..
                    } if update_fd_position => FileResponse::Read(Err(error)),
                    _ => FileResponse::ReadLimited(Err(error)),
                };

                message_bus
                    .send(ToLayer {
                        message_id,
                        layer_id,
                        message: ProxyToLayerMessage::File(message),
                    })
                    .await;
            }

            // If the file is buffered, update `files_data`.
            FileResponse::Seek(Ok(seek)) => {
                let (message_id, layer_id, additional_data) =
                    self.request_queue.pop_front_with_data().ok_or_else(|| {
                        UnexpectedAgentMessage(DaemonMessage::File(FileResponse::Seek(Ok(
                            seek.clone()
                        ))))
                    })?;

                if let AdditionalRequestData::SeekBuffered { fd } = additional_data {
                    let Some(data) = self.buffered_files.get_mut(&fd) else {
                        // File must have been closed from other thread in user application.
                        message_bus
                            .send(ToLayer {
                                message_id,
                                layer_id,
                                message: ProxyToLayerMessage::File(FileResponse::Seek(Err(
                                    ResponseError::NotFound(fd),
                                ))),
                            })
                            .await;
                        return Ok(());
                    };

                    data.fd_position = seek.result_offset;
                }

                message_bus
                    .send(ToLayer {
                        message_id,
                        layer_id,
                        message: ProxyToLayerMessage::File(FileResponse::Seek(Ok(seek))),
                    })
                    .await;
            }

            // Store extra entries in `dirs_data`.
            FileResponse::ReadDirBatch(Ok(batch)) => {
                let (message_id, layer_id) = self.request_queue.pop_front().ok_or_else(|| {
                    UnexpectedAgentMessage(DaemonMessage::File(FileResponse::ReadDirBatch(Ok(
                        batch.clone(),
                    ))))
                })?;

                let Some(data) = self.buffered_dirs.get_mut(&batch.fd) else {
                    // Directory must have been closed from other thread in user application.
                    message_bus
                        .send(ToLayer {
                            message_id,
                            layer_id,
                            message: ProxyToLayerMessage::File(FileResponse::ReadDir(Err(
                                ResponseError::NotFound(batch.fd),
                            ))),
                        })
                        .await;
                    return Ok(());
                };

                let mut entries = batch.dir_entries.into_iter();
                let direntry = entries.next();
                data.buffered_entries = entries;

                message_bus
                    .send(ToLayer {
                        message_id,
                        layer_id,
                        message: ProxyToLayerMessage::File(FileResponse::ReadDir(Ok(
                            ReadDirResponse { direntry },
                        ))),
                    })
                    .await;
            }
            // Convert to XstatFsV2 so that the layer doesn't ever need to deal with the old type.
            FileResponse::XstatFs(res) => {
                let (message_id, layer_id) = self.request_queue.pop_front().ok_or_else(|| {
                    UnexpectedAgentMessage(DaemonMessage::File(FileResponse::XstatFs(res.clone())))
                })?;
                message_bus
                    .send(ToLayer {
                        message_id,
                        layer_id,
                        message: ProxyToLayerMessage::File(FileResponse::XstatFsV2(
                            res.map(Into::into),
                        )),
                    })
                    .await;
            }

            // Doesn't require any special logic.
            other => {
                let (message_id, layer_id) = self
                    .request_queue
                    .pop_front()
                    .ok_or_else(|| UnexpectedAgentMessage(DaemonMessage::File(other.clone())))?;
                message_bus
                    .send(ToLayer {
                        message_id,
                        layer_id,
                        message: ProxyToLayerMessage::File(other),
                    })
                    .await;
            }
        }

        Ok(())
    }

    #[tracing::instrument(level = Level::INFO, skip(message_bus), ret)]
    async fn handle_reconnect(&mut self, message_bus: &mut MessageBus<Self>) {
        let files_to_drop = self
            .remote_files
            .drain()
            .flat_map(|(_, fds)| fds)
            .collect::<HashSet<_>>();
        tracing::debug!(?files_to_drop, "Dropping remote files");
        for fd in files_to_drop {
            self.buffered_files.remove(&fd);
        }

        let directories_to_drop = self
            .remote_dirs
            .drain()
            .flat_map(|(_, fds)| fds)
            .collect::<HashSet<_>>();
        tracing::debug!(?directories_to_drop, "Dropping remote directories");
        for fd in directories_to_drop {
            self.buffered_dirs.remove(&fd);
        }

        let responses = self.reconnect_tracker.agent_lost();
        tracing::debug!(
            num_responses = responses.len(),
            "Flushing error responses to file requests"
        );
        for response in self.reconnect_tracker.agent_lost() {
            message_bus.send(ToLayer::from(response)).await;
        }
    }
}

impl BackgroundTask for FilesProxy {
    type MessageIn = FilesProxyMessage;
    type MessageOut = ProxyMessage;
    type Error = FilesProxyError;

    #[tracing::instrument(level = Level::INFO, name = "files_proxy_main_loop", skip_all, ret, err)]
    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        while let Some(message) = message_bus.recv().await {
            match message {
                FilesProxyMessage::FileReq(message_id, layer_id, request) => {
                    match self
                        .reconnect_tracker
                        .map_request(layer_id, message_id, request)
                    {
                        Ok(None) => {}
                        Err(response) => {
                            message_bus.send(response).await;
                        }
                        Ok(Some(request)) => {
                            self.file_request(request, layer_id, message_id, message_bus)
                                .await
                        }
                    };
                }
                FilesProxyMessage::FileRes(response) => {
                    let response = self.reconnect_tracker.map_response(response);
                    self.file_response(response, message_bus).await?;
                }
                FilesProxyMessage::LayerClosed(closed) => {
                    self.layer_closed(closed, message_bus).await;
                }
                FilesProxyMessage::LayerForked(forked) => self.layer_forked(forked),
                FilesProxyMessage::ProtocolVersion(version) => self.protocol_version(version),
                FilesProxyMessage::ConnectionRefresh => self.handle_reconnect(message_bus).await,
            }
        }

        tracing::debug!("Message bus closed, exiting");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mirrord_intproxy_protocol::{LayerId, ProxyToLayerMessage};
    use mirrord_protocol::{
        file::{
            FdOpenDirRequest, OpenDirResponse, OpenFileRequest, OpenFileResponse,
            OpenOptionsInternal, ReadDirBatchRequest, ReadDirBatchResponse, ReadDirRequest,
            ReadDirResponse, ReadFileRequest, ReadFileResponse, ReadLimitedFileRequest,
            SeekFileRequest, SeekFileResponse, SeekFromInternal,
        },
        ClientMessage, ErrorKindInternal, FileRequest, FileResponse, RemoteIOError, ResponseError,
    };
    use rstest::rstest;
    use semver::Version;

    use super::{FilesProxy, FilesProxyMessage};
    use crate::{
        background_tasks::{BackgroundTasks, TaskSender, TaskUpdate},
        error::ProxyRuntimeError,
        main_tasks::{MainTaskId, ProxyMessage, ToLayer},
    };

    /// Sets up a [`TaskSender`] and [`BackgroundTasks`] for a functioning [`FilesProxy`].
    ///
    /// - `protocol_version`: allows specifying the version of the protocol to use for testing out
    ///   potential mismatches in messages.
    /// - `buffer_reads`: configures buffering readonly files
    async fn setup_proxy(
        protocol_version: Version,
        file_buffer_size: u64,
    ) -> (
        TaskSender<FilesProxy>,
        BackgroundTasks<MainTaskId, ProxyMessage, ProxyRuntimeError>,
    ) {
        let mut tasks: BackgroundTasks<MainTaskId, ProxyMessage, ProxyRuntimeError> =
            Default::default();

        let proxy = tasks.register(
            FilesProxy::new(file_buffer_size),
            MainTaskId::FilesProxy,
            32,
        );

        proxy
            .send(FilesProxyMessage::ProtocolVersion(protocol_version))
            .await;

        (proxy, tasks)
    }

    /// Convenience for opening a dir.
    async fn prepare_dir(
        proxy: &TaskSender<FilesProxy>,
        tasks: &mut BackgroundTasks<MainTaskId, ProxyMessage, ProxyRuntimeError>,
    ) {
        let request = FileRequest::FdOpenDir(FdOpenDirRequest { remote_fd: 0xdad });
        proxy
            .send(FilesProxyMessage::FileReq(0xbad, LayerId(0xa55), request))
            .await;
        let (_, update) = tasks.next().await.unzip();

        assert!(
            matches!(
                update,
                Some(TaskUpdate::Message(ProxyMessage::ToAgent(
                    ClientMessage::FileRequest(FileRequest::FdOpenDir(FdOpenDirRequest {
                        remote_fd: 0xdad
                    }),)
                )))
            ),
            "Mismatched message for `FdOpenDirRequest` {update:?}!"
        );

        let response = FileResponse::OpenDir(Ok(OpenDirResponse { fd: 0xdad }));
        proxy.send(FilesProxyMessage::FileRes(response)).await;
        let (_, update) = tasks.next().await.unzip();

        assert!(
            matches!(
                update,
                Some(TaskUpdate::Message(ProxyMessage::ToLayer(ToLayer {
                    message_id: 0xbad,
                    layer_id: LayerId(0xa55),
                    message: ProxyToLayerMessage::File(FileResponse::OpenDir(Ok(
                        OpenDirResponse { .. }
                    )))
                })))
            ),
            "Mismatched message for `OpenDirResponse` {update:?}!"
        );
    }

    #[tokio::test]
    async fn old_protocol_uses_read_dir_request() {
        let (proxy, mut tasks) = setup_proxy(Version::new(0, 1, 0), 0).await;

        prepare_dir(&proxy, &mut tasks).await;

        let readdir_request = FileRequest::ReadDir(ReadDirRequest { remote_fd: 0xdad });
        proxy
            .send(FilesProxyMessage::FileReq(
                0xbad,
                LayerId(0xa55),
                readdir_request,
            ))
            .await;
        let (_, update) = tasks.next().await.unzip();

        assert!(
            matches!(
                update,
                Some(TaskUpdate::Message(ProxyMessage::ToAgent(
                    ClientMessage::FileRequest(FileRequest::ReadDir(ReadDirRequest { .. }))
                )))
            ),
            "Mismatched message for `ReadDirRequest` {update:?}!"
        );

        let readdir_response = FileResponse::ReadDir(Ok(ReadDirResponse { direntry: None }));
        proxy
            .send(FilesProxyMessage::FileRes(readdir_response))
            .await;
        let (_, update) = tasks.next().await.unzip();

        assert!(
            matches!(
                update,
                Some(TaskUpdate::Message(ProxyMessage::ToLayer(ToLayer {
                    message_id: 0xbad,
                    layer_id: LayerId(0xa55),
                    message: ProxyToLayerMessage::File(FileResponse::ReadDir(Ok(
                        ReadDirResponse { .. }
                    )))
                })))
            ),
            "Mismatched message for `ReadDirResponse` {update:?}!"
        );

        drop(proxy);
        let results = tasks.results().await;
        for (_, result) in results {
            assert!(result.is_ok(), "{result:?}");
        }
    }

    #[tokio::test]
    async fn new_protocol_uses_read_dir_batch_request() {
        let (proxy, mut tasks) = setup_proxy(Version::new(1, 9, 0), 0).await;

        prepare_dir(&proxy, &mut tasks).await;

        let request = FileRequest::ReadDir(ReadDirRequest { remote_fd: 0xdad });
        proxy
            .send(FilesProxyMessage::FileReq(0xbad, LayerId(0xa55), request))
            .await;
        let (_, update) = tasks.next().await.unzip();

        assert!(
            matches!(
                update,
                Some(TaskUpdate::Message(ProxyMessage::ToAgent(
                    ClientMessage::FileRequest(FileRequest::ReadDirBatch(ReadDirBatchRequest {
                        remote_fd: 0xdad,
                        amount: FilesProxy::READDIR_BATCH_SIZE,
                    }))
                )))
            ),
            "Mismatched message for `ReadDirBatchRequest` {update:?}!"
        );

        let response = FileResponse::ReadDirBatch(Ok(ReadDirBatchResponse {
            fd: 0xdad,
            dir_entries: Vec::new(),
        }));
        proxy.send(FilesProxyMessage::FileRes(response)).await;
        let (_, update) = tasks.next().await.unzip();

        assert!(
            matches!(
                update,
                Some(TaskUpdate::Message(ProxyMessage::ToLayer(ToLayer {
                    message_id: 0xbad,
                    layer_id: LayerId(0xa55),
                    message: ProxyToLayerMessage::File(FileResponse::ReadDir(Ok(
                        ReadDirResponse { .. }
                    )))
                })))
            ),
            "Mismatched message for `ReadDirBatchResponse` {update:?}!"
        );

        drop(proxy);
        let results = tasks.results().await;
        for (_, result) in results {
            assert!(result.is_ok(), "{result:?}");
        }
    }

    /// Helper function for opening a file in a running [`FilesProxy`].
    async fn open_file(
        proxy: &TaskSender<FilesProxy>,
        tasks: &mut BackgroundTasks<MainTaskId, ProxyMessage, ProxyRuntimeError>,
        readonly: bool,
    ) -> u64 {
        let message_id = rand::random();
        let fd = rand::random();

        let request = FileRequest::Open(OpenFileRequest {
            path: PathBuf::from("/some/path"),
            open_options: OpenOptionsInternal {
                read: true,
                write: !readonly,
                ..Default::default()
            },
        });
        proxy
            .send(FilesProxyMessage::FileReq(
                message_id,
                LayerId(0),
                request.clone(),
            ))
            .await;
        let update = tasks.next().await.unwrap().1.unwrap_message();
        assert_eq!(
            update,
            ProxyMessage::ToAgent(ClientMessage::FileRequest(request)),
        );

        let response = FileResponse::Open(Ok(OpenFileResponse { fd }));
        proxy
            .send(FilesProxyMessage::FileRes(response.clone()))
            .await;
        let update = tasks.next().await.unwrap().1.unwrap_message();
        assert_eq!(
            update,
            ProxyMessage::ToLayer(ToLayer {
                message_id,
                layer_id: LayerId(0),
                message: ProxyToLayerMessage::File(response),
            })
        );

        fd
    }

    async fn make_read_request(
        proxy: &TaskSender<FilesProxy>,
        tasks: &mut BackgroundTasks<MainTaskId, ProxyMessage, ProxyRuntimeError>,
        remote_fd: u64,
        buffer_size: u64,
        start_from: Option<u64>,
    ) -> ProxyMessage {
        let message_id = rand::random();
        let request = if let Some(start_from) = start_from {
            FileRequest::ReadLimited(ReadLimitedFileRequest {
                remote_fd,
                buffer_size,
                start_from,
            })
        } else {
            FileRequest::Read(ReadFileRequest {
                remote_fd,
                buffer_size,
            })
        };

        proxy
            .send(FilesProxyMessage::FileReq(message_id, LayerId(0), request))
            .await;
        tasks.next().await.unwrap().1.unwrap_message()
    }

    async fn respond_to_read_request(
        proxy: &TaskSender<FilesProxy>,
        tasks: &mut BackgroundTasks<MainTaskId, ProxyMessage, ProxyRuntimeError>,
        data: Vec<u8>,
        limited: bool,
    ) -> ProxyMessage {
        let response = ReadFileResponse {
            read_amount: data.len() as u64,
            bytes: data.into(),
        };
        let response = if limited {
            FileResponse::ReadLimited(Ok(response))
        } else {
            FileResponse::Read(Ok(response))
        };

        proxy.send(FilesProxyMessage::FileRes(response)).await;
        tasks.next().await.unwrap().1.unwrap_message()
    }

    async fn respond_to_read_request_with_err(
        proxy: &TaskSender<FilesProxy>,
        tasks: &mut BackgroundTasks<MainTaskId, ProxyMessage, ProxyRuntimeError>,
        error: ResponseError,
        limited: bool,
    ) -> ProxyMessage {
        let response = if limited {
            FileResponse::ReadLimited(Err(error))
        } else {
            FileResponse::Read(Err(error))
        };

        proxy.send(FilesProxyMessage::FileRes(response)).await;
        tasks.next().await.unwrap().1.unwrap_message()
    }

    #[rstest]
    #[case(true, false)]
    #[case(false, true)]
    #[case(false, false)]
    #[tokio::test]
    async fn reading_from_unbuffered_file(#[case] readonly: bool, #[case] buffering_enabled: bool) {
        let (proxy, mut tasks) = setup_proxy(
            mirrord_protocol::VERSION.clone(),
            buffering_enabled.then_some(4096).unwrap_or_default(),
        )
        .await;

        let fd = open_file(&proxy, &mut tasks, readonly).await;

        let update = make_read_request(&proxy, &mut tasks, fd, 10, None).await;
        assert_eq!(
            update,
            ProxyMessage::ToAgent(ClientMessage::FileRequest(FileRequest::Read(
                ReadFileRequest {
                    remote_fd: fd,
                    buffer_size: 10,
                }
            ))),
        );

        let update = respond_to_read_request(&proxy, &mut tasks, vec![0; 10], false)
            .await
            .unwrap_proxy_to_layer_message();
        assert_eq!(
            update,
            ProxyToLayerMessage::File(FileResponse::Read(Ok(ReadFileResponse {
                bytes: vec![0; 10].into(),
                read_amount: 10,
            }))),
        );

        let update = make_read_request(&proxy, &mut tasks, fd, 1, Some(13)).await;
        assert_eq!(
            update,
            ProxyMessage::ToAgent(ClientMessage::FileRequest(FileRequest::ReadLimited(
                ReadLimitedFileRequest {
                    remote_fd: fd,
                    buffer_size: 1,
                    start_from: 13,
                }
            ))),
        );

        let update = respond_to_read_request(&proxy, &mut tasks, vec![2], true)
            .await
            .unwrap_proxy_to_layer_message();
        assert_eq!(
            update,
            ProxyToLayerMessage::File(FileResponse::ReadLimited(Ok(ReadFileResponse {
                bytes: vec![2].into(),
                read_amount: 1,
            }))),
        );
    }

    #[tokio::test]
    async fn reading_from_buffered_file() {
        let (proxy, mut tasks) = setup_proxy(mirrord_protocol::VERSION.clone(), 4096).await;

        let fd = open_file(&proxy, &mut tasks, true).await;
        let contents = std::iter::repeat(0_u8..=255).flatten();

        let update = make_read_request(&proxy, &mut tasks, fd, 1, None).await;
        assert_eq!(
            update,
            ProxyMessage::ToAgent(ClientMessage::FileRequest(FileRequest::ReadLimited(
                ReadLimitedFileRequest {
                    remote_fd: fd,
                    buffer_size: 4096,
                    start_from: 0,
                }
            ))),
        );

        let data = contents.clone().take(4096).collect::<Vec<_>>();
        let update = respond_to_read_request(&proxy, &mut tasks, data, true)
            .await
            .unwrap_proxy_to_layer_message();
        assert_eq!(
            update,
            ProxyToLayerMessage::File(FileResponse::Read(Ok(ReadFileResponse {
                bytes: vec![0].into(),
                read_amount: 1,
            }))),
        );

        for i in 1..=3 {
            let update = make_read_request(&proxy, &mut tasks, fd, 1, None)
                .await
                .unwrap_proxy_to_layer_message();
            assert_eq!(
                update,
                ProxyToLayerMessage::File(FileResponse::Read(Ok(ReadFileResponse {
                    bytes: vec![i].into(),
                    read_amount: 1,
                }))),
            );
        }

        let expected = contents.clone().skip(256).take(512).collect::<Vec<_>>();
        let update = make_read_request(&proxy, &mut tasks, fd, 512, Some(256))
            .await
            .unwrap_proxy_to_layer_message();
        assert_eq!(
            update,
            ProxyToLayerMessage::File(FileResponse::ReadLimited(Ok(ReadFileResponse {
                bytes: expected.into(),
                read_amount: 512,
            }))),
        );

        let update = make_read_request(&proxy, &mut tasks, fd, 4096 * 2, None).await;
        assert_eq!(
            update,
            ProxyMessage::ToAgent(ClientMessage::FileRequest(FileRequest::ReadLimited(
                ReadLimitedFileRequest {
                    remote_fd: fd,
                    buffer_size: 4096 * 2,
                    start_from: 4,
                }
            ))),
        );

        let data = contents.clone().skip(4).take(4096).collect::<Vec<_>>();
        let update = respond_to_read_request(&proxy, &mut tasks, data.clone(), true)
            .await
            .unwrap_proxy_to_layer_message();
        assert_eq!(
            update,
            ProxyToLayerMessage::File(FileResponse::Read(Ok(ReadFileResponse {
                bytes: data.into(),
                read_amount: 4096,
            }))),
        );

        let seek_request = FileRequest::Seek(SeekFileRequest {
            fd,
            seek_from: SeekFromInternal::Start(444),
        });
        proxy
            .send(FilesProxyMessage::FileReq(
                rand::random(),
                LayerId(0),
                seek_request.clone(),
            ))
            .await;
        let update = tasks.next().await.unwrap().1.unwrap_message();
        assert_eq!(
            update,
            ProxyMessage::ToAgent(ClientMessage::FileRequest(seek_request)),
        );
        let seek_response = FileResponse::Seek(Ok(SeekFileResponse { result_offset: 444 }));
        proxy
            .send(FilesProxyMessage::FileRes(seek_response.clone()))
            .await;
        let update = tasks
            .next()
            .await
            .unwrap()
            .1
            .unwrap_message()
            .unwrap_proxy_to_layer_message();
        assert_eq!(update, ProxyToLayerMessage::File(seek_response),);

        let expected = contents.clone().skip(444).take(10).collect::<Vec<_>>();
        let update = make_read_request(&proxy, &mut tasks, fd, 10, None)
            .await
            .unwrap_proxy_to_layer_message();
        assert_eq!(
            update,
            ProxyToLayerMessage::File(FileResponse::Read(Ok(ReadFileResponse {
                bytes: expected.into(),
                read_amount: 10,
            })))
        );
    }

    #[tokio::test]
    async fn seeking_in_buffered_file() {
        let (proxy, mut tasks) = setup_proxy(mirrord_protocol::VERSION.clone(), 4096).await;

        let fd = open_file(&proxy, &mut tasks, true).await;
        let contents = std::iter::repeat(0_u8..=255).flatten();

        let update = make_read_request(&proxy, &mut tasks, fd, 20, None).await;
        assert_eq!(
            update,
            ProxyMessage::ToAgent(ClientMessage::FileRequest(FileRequest::ReadLimited(
                ReadLimitedFileRequest {
                    remote_fd: fd,
                    buffer_size: 4096,
                    start_from: 0,
                }
            ))),
        );

        let data = contents.clone().take(4096).collect::<Vec<_>>();
        let expected = contents.take(20).collect::<Vec<_>>();
        let update = respond_to_read_request(&proxy, &mut tasks, data, true)
            .await
            .unwrap_proxy_to_layer_message();
        assert_eq!(
            update,
            ProxyToLayerMessage::File(FileResponse::Read(Ok(ReadFileResponse {
                bytes: expected.into(),
                read_amount: 20,
            }))),
        );

        let seek_request = FileRequest::Seek(SeekFileRequest {
            fd,
            seek_from: SeekFromInternal::Current(-30),
        });
        proxy
            .send(FilesProxyMessage::FileReq(
                rand::random(),
                LayerId(0),
                seek_request.clone(),
            ))
            .await;
        let update = tasks
            .next()
            .await
            .unwrap()
            .1
            .unwrap_message()
            .unwrap_proxy_to_layer_message();
        assert_eq!(
            update,
            ProxyToLayerMessage::File(FileResponse::Seek(Err(ResponseError::RemoteIO(
                RemoteIOError {
                    raw_os_error: Some(22),
                    kind: ErrorKindInternal::InvalidInput,
                }
            ))))
        );

        let seek_request = FileRequest::Seek(SeekFileRequest {
            fd,
            seek_from: SeekFromInternal::Current(-10),
        });
        proxy
            .send(FilesProxyMessage::FileReq(
                rand::random(),
                LayerId(0),
                seek_request.clone(),
            ))
            .await;
        let update = tasks.next().await.unwrap().1.unwrap_message();
        assert_eq!(
            update,
            ProxyMessage::ToAgent(ClientMessage::FileRequest(FileRequest::Seek(
                SeekFileRequest {
                    fd,
                    seek_from: SeekFromInternal::Start(10),
                }
            ))),
        );
        let seek_response = FileResponse::Seek(Ok(SeekFileResponse { result_offset: 10 }));
        proxy
            .send(FilesProxyMessage::FileRes(seek_response.clone()))
            .await;
        let update = tasks
            .next()
            .await
            .unwrap()
            .1
            .unwrap_message()
            .unwrap_proxy_to_layer_message();
        assert_eq!(update, ProxyToLayerMessage::File(seek_response),);
    }

    #[tokio::test]
    async fn reading_from_dir() {
        // relevant ticket: MBE-717: intproxy crashes when attempting to `cat` a remote dir
        // test that a ReadLimited response from agent is sent to the layer the same variant as the
        // original request type
        let (proxy, mut tasks) = setup_proxy(mirrord_protocol::VERSION.clone(), 4096).await;

        // create dir - use empty file
        let fd = open_file(&proxy, &mut tasks, true).await;

        // send read request from layer, check that proxy sends readlimited request to agent
        let update = make_read_request(&proxy, &mut tasks, fd, 1, None).await;
        assert_eq!(
            update,
            ProxyMessage::ToAgent(ClientMessage::FileRequest(FileRequest::ReadLimited(
                ReadLimitedFileRequest {
                    remote_fd: fd,
                    buffer_size: 4096,
                    start_from: 0,
                }
            ))),
        );

        // reply from agent with readlimited error, check that proxy sends read response to layer
        let res_error = ResponseError::NotFile(fd);
        let update = respond_to_read_request_with_err(&proxy, &mut tasks, res_error.clone(), true)
            .await
            .unwrap_proxy_to_layer_message();
        assert_eq!(
            update,
            ProxyToLayerMessage::File(FileResponse::Read(Err(res_error))),
        );
    }
}
