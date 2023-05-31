use core::fmt;
use std::{
    fs::File,
    io::SeekFrom,
    os::unix::io::RawFd,
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use dashmap::DashMap;
use libc::{c_int, O_ACCMODE, O_APPEND, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};
/// File operations on remote pod.
///
/// Read-only file operations are enabled by default, you can turn it off by setting
/// `MIRRORD_FILE_RO_OPS` to `false`.
///
///
/// Some file paths and types are ignored by default (bypassed by mirrord, meaning they are
/// opened locally), these are controlled by configuring the [`filter::FileFilter`] with
/// `[FsConfig]`.
#[cfg(target_os = "linux")]
use mirrord_protocol::file::{GetDEnts64Request, GetDEnts64Response};
use mirrord_protocol::{
    file::{
        AccessFileRequest, AccessFileResponse, CloseDirRequest, CloseFileRequest, DirEntryInternal,
        FdOpenDirRequest, OpenDirResponse, OpenFileRequest, OpenFileResponse, OpenOptionsInternal,
        OpenRelativeFileRequest, ReadDirRequest, ReadDirResponse, ReadFileRequest,
        ReadFileResponse, ReadLimitedFileRequest, SeekFileRequest, SeekFileResponse,
        WriteFileRequest, WriteFileResponse, WriteLimitedFileRequest, XstatFsRequest,
        XstatFsResponse, XstatRequest, XstatResponse,
    },
    ClientMessage, FileRequest, FileResponse, RemoteResult,
};
use tokio::sync::mpsc::Sender;
use tracing::{error, trace, warn};

use crate::{
    common::{ResponseChannel, ResponseDeque},
    error::{LayerError, Result},
};

pub(crate) mod filter;
pub(crate) mod hooks;
pub(crate) mod ops;

type LocalFd = RawFd;
type RemoteFd = u64;
type DirStreamFd = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirStream {
    direntry: Box<DirEntryInternal>,
}

/// `OPEN_FILES` is used to track open files and their corrospending remote file descriptor.
/// We use Arc so we can support dup more nicely, this means that if user
/// Opens file `A`, receives fd 1, then dups, receives 2 - both stay open, until both are closed.
/// Previously in such scenario we would close the remote, causing issues.
pub(crate) static OPEN_FILES: LazyLock<DashMap<LocalFd, Arc<ops::RemoteFile>>> =
    LazyLock::new(|| DashMap::with_capacity(4));

/// used just to have a local fd for each remote file.
pub(crate) static mut TEMP_LOCAL_FILES: LazyLock<DashMap<LocalFd, File>> =
    LazyLock::new(|| DashMap::with_capacity(4));

pub(crate) static OPEN_DIRS: LazyLock<DashMap<DirStreamFd, RemoteFd>> =
    LazyLock::new(|| DashMap::with_capacity(4));

/// Extension trait for [`OpenOptionsInternal`], used to convert between `libc`-ish open options and
/// Rust's [`std::fs::OpenOptions`]
pub(crate) trait OpenOptionsInternalExt {
    fn from_flags(flags: c_int) -> Self;
    fn from_mode(mode: String) -> Self;
}

impl OpenOptionsInternalExt for OpenOptionsInternal {
    fn from_flags(flags: c_int) -> Self {
        OpenOptionsInternal {
            read: (flags & O_ACCMODE == O_RDONLY) || (flags & O_ACCMODE == O_RDWR),
            write: (flags & O_ACCMODE == O_WRONLY) || (flags & O_ACCMODE == O_RDWR),
            append: (flags & O_APPEND != 0),
            truncate: (flags & O_TRUNC != 0),
            create: (flags & O_CREAT != 0),
            create_new: false,
        }
    }

    /// WARN: Using the wrong mode is undefined behavior, according to the C standard, we're not
    /// deviating from it.
    fn from_mode(mode: String) -> Self {
        mode.chars()
            .fold(OpenOptionsInternal::default(), |mut open_options, value| {
                match value {
                    'r' => open_options.read = true,
                    'w' => {
                        open_options.write = true;
                        open_options.create = true;
                        open_options.truncate = true;
                    }
                    'a' => {
                        open_options.append = true;
                        open_options.create = true;
                    }
                    '+' => {
                        open_options.read = true;
                        open_options.write = true;
                    }
                    'x' => {
                        open_options.create_new = true;
                    }
                    // Only has meaning for `fmemopen`.
                    'b' => {}
                    invalid => {
                        warn!("Invalid mode for fopen {:#?}", invalid);
                    }
                }

                open_options
            })
    }
}

#[derive(Default)]
pub struct FileHandler {
    /// idea: Replace all VecDeque with HashMap, the assumption order will remain is dangerous :O
    open_queue: ResponseDeque<OpenFileResponse>,
    read_queue: ResponseDeque<ReadFileResponse>,
    read_limited_queue: ResponseDeque<ReadFileResponse>,
    seek_queue: ResponseDeque<SeekFileResponse>,
    write_queue: ResponseDeque<WriteFileResponse>,
    write_limited_queue: ResponseDeque<WriteFileResponse>,
    access_queue: ResponseDeque<AccessFileResponse>,
    xstat_queue: ResponseDeque<XstatResponse>,
    xstatfs_queue: ResponseDeque<XstatFsResponse>,
    opendir_queue: ResponseDeque<OpenDirResponse>,
    readdir_queue: ResponseDeque<ReadDirResponse>,
    #[cfg(target_os = "linux")]
    getdents64_queue: ResponseDeque<GetDEnts64Response>,
}

/// Comfort function for popping oldest request from queue and sending given value into the channel.
#[tracing::instrument(level = "trace", skip(deque))]
fn pop_send<T: fmt::Debug>(deque: &mut ResponseDeque<T>, value: RemoteResult<T>) -> Result<()> {
    deque
        .pop_front()
        .ok_or(LayerError::SendErrorFileResponse)?
        .send(value)
        .map_err(|fail| {
            error!("Failed send operation with {:#?}!", fail);
            LayerError::SendErrorFileResponse
        })
}

impl FileHandler {
    pub(crate) async fn handle_daemon_message(&mut self, message: FileResponse) -> Result<()> {
        use FileResponse::*;
        match message {
            Open(open) => {
                trace!("DaemonMessage::OpenFileResponse {open:#?}!");
                pop_send(&mut self.open_queue, open)
            }
            Read(read) => {
                // The debug message is too big if we just log it directly.
                let _ = read
                    .as_ref()
                    .inspect(|success| trace!("DaemonMessage::ReadFileResponse {:#?}", success))
                    .inspect_err(|fail| trace!("DaemonMessage::ReadFileResponse {:#?}", fail));

                pop_send(&mut self.read_queue, read)
            }
            ReadLimited(read) => {
                // The debug message is too big if we just log it directly.
                let _ = read
                    .as_ref()
                    .inspect(|success| {
                        trace!("DaemonMessage::ReadLimitedFileResponse {:#?}", success)
                    })
                    .inspect_err(|fail| {
                        trace!("DaemonMessage::ReadLimitedFileResponse {:#?}", fail)
                    });

                pop_send(&mut self.read_limited_queue, read).inspect_err(|fail| {
                    error!(
                        "handle_daemon_message -> Failed `pop_send` with {:#?}",
                        fail,
                    )
                })
            }
            Seek(seek) => {
                trace!("DaemonMessage::SeekFileResponse {:#?}!", seek);
                pop_send(&mut self.seek_queue, seek)
            }
            Write(write) => {
                trace!("DaemonMessage::WriteFileResponse {:#?}!", write);
                pop_send(&mut self.write_queue, write)
            }
            Access(access) => {
                trace!("DaemonMessage::AccessFileResponse {:#?}!", access);
                pop_send(&mut self.access_queue, access)
            }
            WriteLimited(write) => {
                let _ = write
                    .as_ref()
                    .inspect(|success| {
                        trace!("DaemonMessage::WriteLimitedFileResponse {:#?}", success)
                    })
                    .inspect_err(|fail| {
                        trace!("DaemonMessage::WriteLimitedFileResponse {:#?}", fail)
                    });

                pop_send(&mut self.write_limited_queue, write).inspect_err(|fail| {
                    error!("handle_daemon_message -> Failed `pop_send` with {fail:#?}")
                })
            }
            Xstat(xstat) => {
                trace!("DaemonMessage::XstatResponse {xstat:#?}!");
                pop_send(&mut self.xstat_queue, xstat)
            }
            XstatFs(xstatfs) => {
                trace!("DaemonMessage::XstatFsResponse {xstatfs:#?}!");
                pop_send(&mut self.xstatfs_queue, xstatfs)
            }
            ReadDir(read_dir) => {
                trace!("DaemonMessage::ReadDirResponse {read_dir:#?}!");
                pop_send(&mut self.readdir_queue, read_dir)
            }
            OpenDir(open_dir) => pop_send(&mut self.opendir_queue, open_dir),
            #[cfg(target_os = "linux")]
            GetDEnts64(getdents64) => {
                trace!("DaemonMessage::GetDEnts64Response {getdents64:#?}!");
                pop_send(&mut self.getdents64_queue, getdents64)
            }
        }
    }

    #[tracing::instrument(level = "trace", skip(self, tx))]
    pub(crate) async fn handle_hook_message(
        &mut self,
        message: FileOperation,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        use FileOperation::*;
        match message {
            Open(open) => self.handle_hook_open(open, tx).await,
            OpenRelative(open_relative) => self.handle_hook_open_relative(open_relative, tx).await,

            Read(read) => self.handle_hook_read(read, tx).await,
            ReadLimited(read) => self.handle_hook_read_limited(read, tx).await,
            Seek(seek) => self.handle_hook_seek(seek, tx).await,
            Write(write) => self.handle_hook_write(write, tx).await,
            WriteLimited(write) => self.handle_hook_write_limited(write, tx).await,
            Close(close) => self.handle_hook_close(close, tx).await,
            Access(access) => self.handle_hook_access(access, tx).await,
            Xstat(xstat) => self.handle_hook_xstat(xstat, tx).await,
            XstatFs(xstatfs) => self.handle_hook_xstatfs(xstatfs, tx).await,
            ReadDir(read_dir) => self.handle_hook_read_dir(read_dir, tx).await,
            FdOpenDir(open_dir) => self.handle_hook_fdopen_dir(open_dir, tx).await,
            CloseDir(close_dir) => self.handle_hook_close_dir(close_dir, tx).await,
            #[cfg(target_os = "linux")]
            GetDEnts64(getdents64) => self.handle_hook_getdents64(getdents64, tx).await,
        }
    }

    #[tracing::instrument(level = "trace", skip(self, tx))]
    async fn handle_hook_open(&mut self, open: Open, tx: &Sender<ClientMessage>) -> Result<()> {
        let Open {
            file_channel_tx,
            path,
            open_options,
        } = open;
        trace!(
            "HookMessage::OpenFileHook path {:#?} | options {:#?}",
            path,
            open_options
        );

        self.open_queue.push_back(file_channel_tx);

        let open_file_request = OpenFileRequest { path, open_options };

        let request = ClientMessage::FileRequest(FileRequest::Open(open_file_request));
        tx.send(request).await.map_err(From::from)
    }

    #[tracing::instrument(level = "trace", skip(self, tx))]
    async fn handle_hook_open_relative(
        &mut self,
        open_relative: OpenRelative,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        let OpenRelative {
            relative_fd,
            path,
            file_channel_tx,
            open_options,
        } = open_relative;
        trace!(
            "HookMessage::OpenRelativeFileHook fd {:#?} | path {:#?} | options {:#?}",
            relative_fd,
            path,
            open_options
        );

        self.open_queue.push_back(file_channel_tx);

        let open_relative_file_request = OpenRelativeFileRequest {
            relative_fd,
            path,
            open_options,
        };

        let request =
            ClientMessage::FileRequest(FileRequest::OpenRelative(open_relative_file_request));
        tx.send(request).await.map_err(From::from)
    }

    #[tracing::instrument(level = "trace", skip(self, tx))]
    async fn handle_hook_read(
        &mut self,
        read: Read<ReadFileResponse>,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        let Read {
            remote_fd,
            buffer_size,
            file_channel_tx,
            ..
        } = read;

        self.read_queue.push_back(file_channel_tx);

        let read_file_request = ReadFileRequest {
            remote_fd,
            buffer_size,
        };

        let request = ClientMessage::FileRequest(FileRequest::Read(read_file_request));
        tx.send(request).await.map_err(From::from)
    }

    #[tracing::instrument(level = "trace", skip(self, tx))]
    async fn handle_hook_read_limited(
        &mut self,
        read: Read<ReadFileResponse>,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        let Read {
            remote_fd,
            buffer_size,
            start_from,
            file_channel_tx,
        } = read;

        self.read_limited_queue.push_back(file_channel_tx);

        let read_file_request = ReadLimitedFileRequest {
            remote_fd,
            buffer_size,
            start_from,
        };

        let request = ClientMessage::FileRequest(FileRequest::ReadLimited(read_file_request));
        tx.send(request).await.map_err(From::from)
    }

    async fn handle_hook_seek(&mut self, seek: Seek, tx: &Sender<ClientMessage>) -> Result<()> {
        let Seek {
            remote_fd: fd,
            seek_from,
            file_channel_tx,
        } = seek;
        trace!(
            "HookMessage::SeekFileHook fd {:#?} | seek_from {:#?}",
            fd,
            seek_from
        );

        self.seek_queue.push_back(file_channel_tx);

        let seek_file_request = SeekFileRequest {
            fd,
            seek_from: seek_from.into(),
        };

        let request = ClientMessage::FileRequest(FileRequest::Seek(seek_file_request));
        tx.send(request).await.map_err(From::from)
    }

    async fn handle_hook_write(
        &mut self,
        write: Write<WriteFileResponse>,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        let Write {
            remote_fd: fd,
            write_bytes,
            file_channel_tx,
            ..
        } = write;
        trace!(
            "HookMessage::WriteFileHook fd {:#?} | length {:#?}",
            fd,
            write_bytes.len()
        );

        self.write_queue.push_back(file_channel_tx);

        let write_file_request = WriteFileRequest { fd, write_bytes };

        let request = ClientMessage::FileRequest(FileRequest::Write(write_file_request));
        tx.send(request).await.map_err(From::from)
    }

    #[tracing::instrument(level = "trace", skip(self, tx))]
    async fn handle_hook_write_limited(
        &mut self,
        write: Write<WriteFileResponse>,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        let Write {
            remote_fd,
            start_from,
            write_bytes,
            file_channel_tx,
        } = write;

        self.write_limited_queue.push_back(file_channel_tx);

        let write_file_request = WriteLimitedFileRequest {
            remote_fd,
            start_from,
            write_bytes,
        };

        let request = ClientMessage::FileRequest(FileRequest::WriteLimited(write_file_request));
        tx.send(request).await.map_err(From::from)
    }

    async fn handle_hook_close(&mut self, close: Close, tx: &Sender<ClientMessage>) -> Result<()> {
        let Close { fd } = close;
        trace!("HookMessage::CloseFileHook fd {:#?}", fd);

        let close_file_request = CloseFileRequest { fd };

        let request = ClientMessage::FileRequest(FileRequest::Close(close_file_request));
        tx.send(request).await.map_err(From::from)
    }

    async fn handle_hook_close_dir(
        &mut self,
        close: CloseDir,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        let CloseDir { fd } = close;
        trace!("HookMessage::CloseDirHook fd {:#?}", fd);

        let close_dir_request = CloseDirRequest { remote_fd: fd };

        let request = ClientMessage::FileRequest(FileRequest::CloseDir(close_dir_request));
        tx.send(request).await.map_err(From::from)
    }

    async fn handle_hook_access(
        &mut self,
        access: Access,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        let Access {
            path: pathname,
            mode,
            file_channel_tx,
        } = access;

        trace!(
            "HookMessage::AccessFileHook pathname {:#?} | mode {:#?}",
            pathname,
            mode
        );

        self.access_queue.push_back(file_channel_tx);

        let access_file_request = AccessFileRequest { pathname, mode };

        let request = ClientMessage::FileRequest(FileRequest::Access(access_file_request));
        tx.send(request).await.map_err(From::from)
    }

    #[tracing::instrument(level = "trace", skip(self, tx))]
    async fn handle_hook_xstat(&mut self, xstat: Xstat, tx: &Sender<ClientMessage>) -> Result<()> {
        let Xstat {
            path,
            fd,
            follow_symlink,
            file_channel_tx,
        } = xstat;
        self.xstat_queue.push_back(file_channel_tx);

        let xstat_request = XstatRequest {
            path,
            fd,
            follow_symlink,
        };

        let request = ClientMessage::FileRequest(FileRequest::Xstat(xstat_request));
        tx.send(request).await.map_err(From::from)
    }

    async fn handle_hook_xstatfs(
        &mut self,
        xstat: XstatFs,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        let XstatFs { fd, fs_channel_tx } = xstat;

        self.xstatfs_queue.push_back(fs_channel_tx);

        let xstatfs_request = XstatFsRequest { fd };

        let request = ClientMessage::FileRequest(FileRequest::XstatFs(xstatfs_request));
        tx.send(request).await.map_err(From::from)
    }

    #[tracing::instrument(level = "trace", skip(self, tx))]
    async fn handle_hook_fdopen_dir(
        &mut self,
        open_dir: FdOpenDir,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        let FdOpenDir {
            remote_fd,
            dir_channel_tx,
        } = open_dir;

        self.opendir_queue.push_back(dir_channel_tx);

        let open_dir_request = FdOpenDirRequest { remote_fd };

        let request = ClientMessage::FileRequest(FileRequest::FdOpenDir(open_dir_request));
        tx.send(request).await.map_err(From::from)
    }

    #[tracing::instrument(level = "trace", skip(self, tx))]
    async fn handle_hook_read_dir(
        &mut self,
        read_dir: ReadDir,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        let ReadDir {
            remote_fd,
            dir_channel_tx,
        } = read_dir;

        self.readdir_queue.push_back(dir_channel_tx);

        let read_dir_request = ReadDirRequest { remote_fd };

        let request = ClientMessage::FileRequest(FileRequest::ReadDir(read_dir_request));
        tx.send(request).await.map_err(From::from)
    }

    #[cfg(target_os = "linux")]
    #[tracing::instrument(level = "trace", skip(self, tx))]
    async fn handle_hook_getdents64(
        &mut self,
        getdents64: GetDEnts64,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        let GetDEnts64 {
            remote_fd,
            buffer_size,
            dents_tx,
        } = getdents64;

        self.getdents64_queue.push_back(dents_tx);

        let getdents_request = GetDEnts64Request {
            remote_fd,
            buffer_size,
        };

        let request = ClientMessage::FileRequest(FileRequest::GetDEnts64(getdents_request));
        tx.send(request).await.map_err(From::from)
    }
}

#[derive(Debug)]
pub struct Open {
    pub(crate) path: PathBuf,
    pub(crate) file_channel_tx: ResponseChannel<OpenFileResponse>,
    pub(crate) open_options: OpenOptionsInternal,
}

#[derive(Debug)]
pub struct OpenRelative {
    pub(crate) relative_fd: u64,
    pub(crate) path: PathBuf,
    pub(crate) file_channel_tx: ResponseChannel<OpenFileResponse>,
    pub(crate) open_options: OpenOptionsInternal,
}

#[derive(Debug)]
pub struct Read<T> {
    pub(crate) remote_fd: u64,
    pub(crate) buffer_size: u64,
    // TODO(alex): Only used for `pread`.
    pub(crate) start_from: u64,
    pub(crate) file_channel_tx: ResponseChannel<T>,
}

#[derive(Debug)]
pub struct Seek {
    pub(crate) remote_fd: u64,
    pub(crate) seek_from: SeekFrom,
    pub(crate) file_channel_tx: ResponseChannel<SeekFileResponse>,
}

#[derive(Debug)]
pub struct Write<T> {
    pub(crate) remote_fd: u64,
    pub(crate) write_bytes: Vec<u8>,
    // Only used for `pwrite`.
    pub(crate) start_from: u64,
    pub(crate) file_channel_tx: ResponseChannel<T>,
}

#[derive(Debug)]
pub struct Close {
    pub(crate) fd: u64,
}

#[derive(Debug)]
pub struct Access {
    pub(crate) path: PathBuf,
    pub(crate) mode: u8,
    pub(crate) file_channel_tx: ResponseChannel<AccessFileResponse>,
}

#[derive(Debug)]
pub struct Xstat {
    pub(crate) path: Option<PathBuf>,
    pub(crate) fd: Option<RemoteFd>,
    pub(crate) follow_symlink: bool,
    pub(crate) file_channel_tx: ResponseChannel<XstatResponse>,
}

#[derive(Debug)]
pub struct XstatFs {
    pub(crate) fd: RemoteFd,
    pub(crate) fs_channel_tx: ResponseChannel<XstatFsResponse>,
}

#[derive(Debug)]
pub struct ReadDir {
    pub(crate) remote_fd: u64,
    pub(crate) dir_channel_tx: ResponseChannel<ReadDirResponse>,
}

#[derive(Debug)]
pub struct FdOpenDir {
    pub(crate) remote_fd: u64,
    pub(crate) dir_channel_tx: ResponseChannel<OpenDirResponse>,
}

#[derive(Debug)]
pub struct CloseDir {
    pub(crate) fd: u64,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct GetDEnts64 {
    pub(crate) remote_fd: u64,
    pub(crate) buffer_size: u64,
    pub(crate) dents_tx: ResponseChannel<GetDEnts64Response>,
}

#[derive(Debug)]
pub enum FileOperation {
    Open(Open),
    OpenRelative(OpenRelative),
    Read(Read<ReadFileResponse>),
    ReadLimited(Read<ReadFileResponse>),
    Write(Write<WriteFileResponse>),
    WriteLimited(Write<WriteFileResponse>),
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
