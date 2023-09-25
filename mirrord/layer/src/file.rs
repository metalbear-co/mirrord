use core::fmt;
use std::{
    io::SeekFrom,
    os::unix::io::RawFd,
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use dashmap::DashMap;
use libc::{c_int, O_ACCMODE, O_APPEND, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};
use mirrord_intproxy::protocol::hook::RemoteFd;
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
pub(crate) mod open_dirs;
pub(crate) mod ops;

type LocalFd = RawFd;
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
