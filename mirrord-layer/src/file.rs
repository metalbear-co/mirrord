use core::fmt;
use std::{
    collections::HashMap,
    env,
    io::SeekFrom,
    os::unix::io::RawFd,
    path::PathBuf,
    sync::{LazyLock, Mutex},
};

use futures::SinkExt;
use libc::{c_int, O_ACCMODE, O_APPEND, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};
use mirrord_protocol::{
    AccessFileRequest, AccessFileResponse, ClientCodec, ClientMessage, CloseFileRequest,
    CloseFileResponse, FileRequest, FileResponse, OpenFileRequest, OpenFileResponse,
    OpenOptionsInternal, OpenRelativeFileRequest, ReadFileRequest, ReadFileResponse,
    ReadLineFileRequest, ReadLineFileResponse, RemoteResult, SeekFileRequest, SeekFileResponse,
    WriteFileRequest, WriteFileResponse,
};
use regex::RegexSet;
use tracing::{debug, error, warn};

use crate::{
    common::{ResponseChannel, ResponseDeque},
    error::{LayerError, Result},
};

pub(crate) mod hooks;
pub(crate) mod ops;

/// Regex that ignores system files + files in the current working directory.
static IGNORE_FILES: LazyLock<RegexSet> = LazyLock::new(|| {
    // To handle the problem of injecting `open` and friends into project runners (like in a call to
    // `node app.js`, or `cargo run app`), we're ignoring files from the current working directory.
    let current_dir = env::current_dir().unwrap();

    let set = RegexSet::new([
        r".*\.so",
        r".*\.d",
        r".*\.pyc",
        r".*\.py",
        r".*\.js",
        r".*\.pth",
        r".*\.plist",
        r".*venv\.cfg",
        r"^/proc/.*",
        r"^/sys/.*",
        r"^/lib/.*",
        r"^/etc/.*",
        r"^/usr/.*",
        r"^/dev/.*",
        r"^/opt/.*",
        r"^/home/iojs/.*",
        r"^/home/runner/.*",
        // TODO: `node` searches for this file in multiple directories, bypassing some of our
        // ignore regexes, maybe other "project runners" will do the same.
        r".*/package.json",
        &current_dir.to_string_lossy(),
    ])
    .unwrap();

    set
});

type LocalFd = RawFd;
type RemoteFd = usize;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RemoteFile {
    fd: RawFd,
}

pub(crate) static OPEN_FILES: LazyLock<Mutex<HashMap<LocalFd, RemoteFd>>> =
    LazyLock::new(|| Mutex::new(HashMap::with_capacity(4)));

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
    read_line_queue: ResponseDeque<ReadLineFileResponse>,
    seek_queue: ResponseDeque<SeekFileResponse>,
    write_queue: ResponseDeque<WriteFileResponse>,
    close_queue: ResponseDeque<CloseFileResponse>,
    access_queue: ResponseDeque<AccessFileResponse>,
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
        }
    }

    #[tracing::instrument(level = "trace", skip(self, codec))]
    pub(crate) async fn handle_hook_message(
        &mut self,
        message: HookMessageFile,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<()> {
        use HookMessageFile::*;
        match message {
            Open(open) => self.handle_hook_open(open, codec).await,
            OpenRelative(open_relative) => {
                self.handle_hook_open_relative(open_relative, codec).await
            }

            Read(read) => self.handle_hook_read(read, codec).await,
            ReadLine(read) => self.handle_hook_read_line(read, codec).await,
            Seek(seek) => self.handle_hook_seek(seek, codec).await,
            Write(write) => self.handle_hook_write(write, codec).await,
            Close(close) => self.handle_hook_close(close, codec).await,
            Access(access) => self.handle_hook_access(access, codec).await,
        }
    }

    #[tracing::instrument(level = "trace", skip(self, codec))]
    async fn handle_hook_open(
        &mut self,
        open: Open,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<()> {
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
        codec.send(request).await.map_err(From::from)
    }

    #[tracing::instrument(level = "trace", skip(self, codec))]
    async fn handle_hook_open_relative(
        &mut self,
        open_relative: OpenRelative,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
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
        codec.send(request).await.map_err(From::from)
    }

    #[tracing::instrument(level = "trace", skip(self, codec))]
    async fn handle_hook_read(
        &mut self,
        read: Read,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<()> {
        let Read {
            fd,
            buffer_size,
            file_channel_tx,
        } = read;

        self.read_queue.push_back(file_channel_tx);

        let read_file_request = ReadFileRequest { fd, buffer_size };

        let request = ClientMessage::FileRequest(FileRequest::Read(read_file_request));
        codec.send(request).await.map_err(From::from)
    }

    #[tracing::instrument(level = "trace", skip(self, codec))]
    async fn handle_hook_read_line(
        &mut self,
        read_line: ReadLine,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<()> {
        let ReadLine {
            fd,
            buffer_size,
            file_channel_tx,
        } = read_line;

        self.read_line_queue.push_back(file_channel_tx);

        let read_file_request = ReadLineFileRequest { fd, buffer_size };

        let request = ClientMessage::FileRequest(FileRequest::ReadLine(read_file_request));
        codec.send(request).await.map_err(From::from)
    }

    async fn handle_hook_seek(
        &mut self,
        seek: Seek,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<()> {
        let Seek {
            fd,
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
        codec.send(request).await.map_err(From::from)
    }

    async fn handle_hook_write(
        &mut self,
        write: Write,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<()> {
        let Write {
            fd,
            write_bytes,
            file_channel_tx,
        } = write;
        debug!(
            "HookMessage::WriteFileHook fd {:#?} | length {:#?}",
            fd,
            write_bytes.len()
        );

        self.write_queue.push_back(file_channel_tx);

        let write_file_request = WriteFileRequest { fd, write_bytes };

        let request = ClientMessage::FileRequest(FileRequest::Write(write_file_request));
        codec.send(request).await.map_err(From::from)
    }

    async fn handle_hook_close(
        &mut self,
        close: Close,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<()> {
        let Close {
            fd,
            file_channel_tx,
        } = close;
        debug!("HookMessage::CloseFileHook fd {:#?}", fd);

        self.close_queue.push_back(file_channel_tx);

        let close_file_request = CloseFileRequest { fd };

        let request = ClientMessage::FileRequest(FileRequest::Close(close_file_request));
        codec.send(request).await.map_err(From::from)
    }

    async fn handle_hook_access(
        &mut self,
        access: Access,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<()> {
        let Access {
            pathname,
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
        codec.send(request).await.map_err(From::from)
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
    pub(crate) relative_fd: usize,
    pub(crate) path: PathBuf,
    pub(crate) file_channel_tx: ResponseChannel<OpenFileResponse>,
    pub(crate) open_options: OpenOptionsInternal,
}

#[derive(Debug)]
pub struct Read {
    pub(crate) fd: usize,
    pub(crate) buffer_size: usize,
    pub(crate) file_channel_tx: ResponseChannel<ReadFileResponse>,
}

#[derive(Debug)]
pub struct ReadLine {
    pub(crate) fd: usize,
    pub(crate) buffer_size: usize,
    pub(crate) file_channel_tx: ResponseChannel<ReadLineFileResponse>,
}

#[derive(Debug)]
pub struct Seek {
    pub(crate) fd: usize,
    pub(crate) seek_from: SeekFrom,
    pub(crate) file_channel_tx: ResponseChannel<SeekFileResponse>,
}

pub struct Write {
    pub(crate) fd: usize,
    pub(crate) write_bytes: Vec<u8>,
    pub(crate) file_channel_tx: ResponseChannel<WriteFileResponse>,
}

impl fmt::Debug for Write {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Write")
            .field("fd", &self.fd)
            .field("write_bytes (length)", &self.write_bytes.len())
            .field("file_channel_tx", &self.file_channel_tx)
            .finish()
    }
}

#[derive(Debug)]
pub struct Close {
    pub(crate) fd: usize,
    pub(crate) file_channel_tx: ResponseChannel<CloseFileResponse>,
}

#[derive(Debug)]
pub struct Access {
    pub(crate) pathname: PathBuf,
    pub(crate) mode: u8,
    pub(crate) file_channel_tx: ResponseChannel<AccessFileResponse>,
}

#[derive(Debug)]
pub enum HookMessageFile {
    Open(Open),
    OpenRelative(OpenRelative),
    Read(Read),
    ReadLine(ReadLine),
    Seek(Seek),
    Write(Write),
    Close(Close),
    Access(Access),
}
