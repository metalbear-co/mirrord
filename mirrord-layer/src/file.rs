use std::{
    collections::{HashMap, VecDeque},
    env,
    io::SeekFrom,
    os::unix::io::RawFd,
    path::PathBuf,
    sync::{LazyLock, Mutex},
};

use futures::SinkExt;
use libc::{c_int, O_ACCMODE, O_APPEND, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};
use mirrord_protocol::{
    ClientCodec, ClientMessage, CloseFileRequest, CloseFileResponse, FileRequest, FileResponse,
    OpenFileRequest, OpenFileResponse, OpenOptionsInternal, OpenRelativeFileRequest,
    ReadFileRequest, ReadFileResponse, RemoteResult, SeekFileRequest, SeekFileResponse,
    WriteFileRequest, WriteFileResponse,
};
use regex::RegexSet;
use tracing::{debug, error, warn};

use crate::{common::ResponseChannel, error::LayerError};

pub(crate) mod hooks;
pub(crate) mod ops;

/// Regex that ignores system files + files in the current working directory.
static IGNORE_FILES: LazyLock<RegexSet> = LazyLock::new(|| {
    // To handle the problem of injecting `open` and friends into project runners (like in a call to
    // `node app.js`, or `cargo run app`), we're ignoring files from the current working directory.
    let current_dir = env::current_dir().unwrap();

    let set = RegexSet::new(&[
        r".*\.so",
        r".*\.d",
        r".*\.pyc",
        r".*\.py",
        r".*\.js",
        r".*\.pth",
        r"^/proc/.*",
        r"^/sys/.*",
        r"^/lib/.*",
        r"^/etc/.*",
        r"^/usr/.*",
        r"^/dev/.*",
        r"^/opt/.*",
        r"^/home/iojs/.*",
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
type ResponseDeque<T> = VecDeque<ResponseChannel<T>>;

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
    seek_queue: ResponseDeque<SeekFileResponse>,
    write_queue: ResponseDeque<WriteFileResponse>,
    close_queue: ResponseDeque<CloseFileResponse>,
}

/// Comfort function for popping oldest request from queue and sending given value into the channel.
fn pop_send<T>(deque: &mut ResponseDeque<T>, value: RemoteResult<T>) -> Result<(), LayerError> {
    deque
        .pop_front()
        .ok_or(LayerError::SendErrorFileResponse)?
        .send(value)
        .map_err(|_| LayerError::SendErrorFileResponse)
}

impl FileHandler {
    pub async fn handle_daemon_message(&mut self, message: FileResponse) -> Result<(), LayerError> {
        use FileResponse::*;
        match message {
            Open(open) => {
                debug!("DaemonMessage::OpenFileResponse {open:#?}!");
                pop_send(&mut self.open_queue, open)
            }
            Read(read) => {
                // The debug message is too big if we just log it directly.
                let file_response = read
                    .inspect(|success| {
                        debug!("DaemonMessage::ReadFileResponse {:#?}", success.read_amount)
                    })
                    .inspect_err(|fail| error!("DaemonMessage::ReadFileResponse {:#?}", fail));

                pop_send(&mut self.read_queue, file_response)
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
        }
    }

    pub async fn handle_hook_message(
        &mut self,
        message: HookMessageFile,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<(), LayerError> {
        use HookMessageFile::*;
        match message {
            Open(open) => self.handle_hook_open(open, codec).await,
            OpenRelative(open_relative) => {
                self.handle_hook_open_relative(open_relative, codec).await
            }

            Read(read) => self.handle_hook_read(read, codec).await,
            Seek(seek) => self.handle_hook_seek(seek, codec).await,
            Write(write) => self.handle_hook_write(write, codec).await,
            Close(close) => self.handle_hook_close(close, codec).await,
        }
    }

    async fn handle_hook_open(
        &mut self,
        open: Open,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<(), LayerError> {
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
    async fn handle_hook_open_relative(
        &mut self,
        open_relative: OpenRelative,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<(), LayerError> {
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

    async fn handle_hook_read(
        &mut self,
        read: Read,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<(), LayerError> {
        let Read {
            fd,
            buffer_size,
            file_channel_tx,
        } = read;
        debug!(
            "HookMessage::ReadFileHook fd {:#?} | buffer_size {:#?}",
            fd, buffer_size
        );

        self.read_queue.push_back(file_channel_tx);

        let read_file_request = ReadFileRequest { fd, buffer_size };

        debug!(
            "HookMessage::ReadFileHook read_file_request {:#?}",
            read_file_request
        );

        let request = ClientMessage::FileRequest(FileRequest::Read(read_file_request));
        codec.send(request).await.map_err(From::from)
    }

    async fn handle_hook_seek(
        &mut self,
        seek: Seek,
        codec: &mut actix_codec::Framed<
            impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
            ClientCodec,
        >,
    ) -> Result<(), LayerError> {
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
    ) -> Result<(), LayerError> {
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
    ) -> Result<(), LayerError> {
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
pub struct Seek {
    pub(crate) fd: usize,
    pub(crate) seek_from: SeekFrom,
    pub(crate) file_channel_tx: ResponseChannel<SeekFileResponse>,
}

#[derive(Debug)]
pub struct Write {
    pub(crate) fd: usize,
    pub(crate) write_bytes: Vec<u8>,
    pub(crate) file_channel_tx: ResponseChannel<WriteFileResponse>,
}

#[derive(Debug)]
pub struct Close {
    pub(crate) fd: usize,
    pub(crate) file_channel_tx: ResponseChannel<CloseFileResponse>,
}

#[derive(Debug)]
pub enum HookMessageFile {
    Open(Open),
    OpenRelative(OpenRelative),
    Read(Read),
    Seek(Seek),
    Write(Write),
    Close(Close),
}
