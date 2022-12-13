/// File operations on remote pod.
///
/// Read-only file operations are enabled by default, you can turn it off by setting
/// `MIRRORD_FILE_RO_OPS` to `false`.
///
/// To enable read-write file operations, set `MIRRORD_FILE_OPS` to `true.
///
/// Some file paths and types are ignored by default (bypassed by mirrord, meaning they are
/// open locally), these are controlled by configuring the [`filter::FileFilter`] with either
/// `MIRRORD_FILE_FILTER_INCLUDE` or `MIRRORD_FILE_FILTER_EXCLUDE` (the later adds aditional
/// exclusions to the default).
use core::fmt;
use std::{
    collections::HashMap,
    io::SeekFrom,
    os::unix::io::RawFd,
    path::PathBuf,
    sync::{LazyLock, Mutex},
};

use libc::{c_int, O_ACCMODE, O_APPEND, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};
use mirrord_protocol::{
    file::{XstatRequest, XstatResponse},
    AccessFileRequest, AccessFileResponse, ClientMessage, CloseFileRequest, CloseFileResponse,
    FileRequest, FileResponse, OpenFileRequest, OpenFileResponse, OpenOptionsInternal,
    OpenRelativeFileRequest, ReadFileRequest, ReadFileResponse, ReadLimitedFileRequest,
    ReadLineFileRequest, RemoteResult, SeekFileRequest, SeekFileResponse, WriteFileRequest,
    WriteFileResponse, WriteLimitedFileRequest,
};
use tokio::sync::mpsc::Sender;
use tracing::{debug, error, warn};

use crate::{
    common::{ResponseChannel, ResponseDeque},
    error::{LayerError, Result},
};

pub(crate) mod filter;
pub(crate) mod hooks;
pub(crate) mod ops;

type LocalFd = RawFd;
type RemoteFd = u64;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RemoteFile {
    fd: RawFd,
}

pub(crate) static OPEN_FILES: LazyLock<Mutex<HashMap<LocalFd, RemoteFd>>> =
    LazyLock::new(|| Mutex::new(HashMap::with_capacity(4)));

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
    read_line_queue: ResponseDeque<ReadFileResponse>,
    read_limited_queue: ResponseDeque<ReadFileResponse>,
    seek_queue: ResponseDeque<SeekFileResponse>,
    write_queue: ResponseDeque<WriteFileResponse>,
    write_limited_queue: ResponseDeque<WriteFileResponse>,
    close_queue: ResponseDeque<CloseFileResponse>,
    access_queue: ResponseDeque<AccessFileResponse>,
    xstat_queue: ResponseDeque<XstatResponse>,
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
                debug!("DaemonMessage::OpenFileResponse {open:#?}!");
                pop_send(&mut self.open_queue, open)
            }
            Read(read) => {
                // The debug message is too big if we just log it directly.
                let _ = read
                    .as_ref()
                    .inspect(|success| debug!("DaemonMessage::ReadFileResponse {:#?}", success))
                    .inspect_err(|fail| error!("DaemonMessage::ReadFileResponse {:#?}", fail));

                pop_send(&mut self.read_queue, read)
            }
            ReadLine(read) => {
                // The debug message is too big if we just log it directly.
                let _ = read
                    .as_ref()
                    .inspect(|success| debug!("DaemonMessage::ReadLineFileResponse {:#?}", success))
                    .inspect_err(|fail| error!("DaemonMessage::ReadLineFileResponse {:#?}", fail));

                pop_send(&mut self.read_line_queue, read).inspect_err(|fail| {
                    error!(
                        "handle_daemon_message -> Failed `pop_send` with {:#?}",
                        fail,
                    )
                })
            }
            ReadLimited(read) => {
                // The debug message is too big if we just log it directly.
                let _ = read
                    .as_ref()
                    .inspect(|success| {
                        debug!("DaemonMessage::ReadLimitedFileResponse {:#?}", success)
                    })
                    .inspect_err(|fail| {
                        error!("DaemonMessage::ReadLimitedFileResponse {:#?}", fail)
                    });

                pop_send(&mut self.read_limited_queue, read).inspect_err(|fail| {
                    error!(
                        "handle_daemon_message -> Failed `pop_send` with {:#?}",
                        fail,
                    )
                })
            }
            Seek(seek) => {
                debug!("DaemonMessage::SeekFileResponse {:#?}!", seek);
                pop_send(&mut self.seek_queue, seek)
            }
            Write(write) => {
                debug!("DaemonMessage::WriteFileResponse {:#?}!", write);
                pop_send(&mut self.write_queue, write)
            }
            Close(close) => {
                debug!("DaemonMessage::CloseFileResponse {:#?}!", close);
                pop_send(&mut self.close_queue, close)
            }
            Access(access) => {
                debug!("DaemonMessage::AccessFileResponse {:#?}!", access);
                pop_send(&mut self.access_queue, access)
            }
            WriteLimited(write) => {
                let _ = write
                    .as_ref()
                    .inspect(|success| {
                        debug!("DaemonMessage::WriteLimitedFileResponse {:#?}", success)
                    })
                    .inspect_err(|fail| {
                        error!("DaemonMessage::WriteLimitedFileResponse {:#?}", fail)
                    });

                pop_send(&mut self.write_limited_queue, write).inspect_err(|fail| {
                    error!(
                        "handle_daemon_message -> Failed `pop_send` with {:#?}",
                        fail,
                    )
                })
            }
            Xstat(xstat) => {
                debug!("DaemonMessage::XstatResponse {:#?}!", xstat);
                pop_send(&mut self.xstat_queue, xstat)
            }
        }
    }

    #[tracing::instrument(level = "trace", skip(self, tx))]
    pub(crate) async fn handle_hook_message(
        &mut self,
        message: HookMessageFile,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        use HookMessageFile::*;
        match message {
            Open(open) => self.handle_hook_open(open, tx).await,
            OpenRelative(open_relative) => self.handle_hook_open_relative(open_relative, tx).await,

            Read(read) => self.handle_hook_read(read, tx).await,
            ReadLine(read) => self.handle_hook_read_line(read, tx).await,
            ReadLimited(read) => self.handle_hook_read_limited(read, tx).await,
            Seek(seek) => self.handle_hook_seek(seek, tx).await,
            Write(write) => self.handle_hook_write(write, tx).await,
            WriteLimited(write) => self.handle_hook_write_limited(write, tx).await,
            Close(close) => self.handle_hook_close(close, tx).await,
            Access(access) => self.handle_hook_access(access, tx).await,
            Xstat(xstat) => self.handle_hook_xstat(xstat, tx).await,
        }
    }

    #[tracing::instrument(level = "trace", skip(self, tx))]
    async fn handle_hook_open(&mut self, open: Open, tx: &Sender<ClientMessage>) -> Result<()> {
        let Open {
            file_channel_tx,
            path,
            open_options,
        } = open;
        debug!(
            "HookMessage::OpenFileHook path {:#?} | options {:#?}",
            path, open_options
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
        debug!(
            "HookMessage::OpenRelativeFileHook fd {:#?} | path {:#?} | options {:#?}",
            relative_fd, path, open_options
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
    async fn handle_hook_read_line(
        &mut self,
        read_line: Read<ReadFileResponse>,
        tx: &Sender<ClientMessage>,
    ) -> Result<()> {
        let Read {
            remote_fd,
            buffer_size,
            file_channel_tx,
            ..
        } = read_line;

        self.read_line_queue.push_back(file_channel_tx);

        let read_file_request = ReadLineFileRequest {
            remote_fd,
            buffer_size,
        };

        let request = ClientMessage::FileRequest(FileRequest::ReadLine(read_file_request));
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
        debug!(
            "HookMessage::SeekFileHook fd {:#?} | seek_from {:#?}",
            fd, seek_from
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
        debug!(
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
        let Close {
            fd,
            file_channel_tx,
        } = close;
        debug!("HookMessage::CloseFileHook fd {:#?}", fd);

        self.close_queue.push_back(file_channel_tx);

        let close_file_request = CloseFileRequest { fd };

        let request = ClientMessage::FileRequest(FileRequest::Close(close_file_request));
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

        debug!(
            "HookMessage::AccessFileHook pathname {:#?} | mode {:#?}",
            pathname, mode
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
    pub(crate) file_channel_tx: ResponseChannel<CloseFileResponse>,
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
pub enum HookMessageFile {
    Open(Open),
    OpenRelative(OpenRelative),
    Read(Read<ReadFileResponse>),
    ReadLine(Read<ReadFileResponse>),
    ReadLimited(Read<ReadFileResponse>),
    Write(Write<WriteFileResponse>),
    WriteLimited(Write<WriteFileResponse>),
    Seek(Seek),
    Close(Close),
    Access(Access),
    Xstat(Xstat),
}
