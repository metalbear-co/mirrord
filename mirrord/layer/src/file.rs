use std::{
    collections::HashMap,
    os::unix::io::RawFd,
    sync::{Arc, LazyLock, Mutex},
};

use libc::{c_int, O_ACCMODE, O_APPEND, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};
use mirrord_protocol::file::{
    AccessFileRequest, CloseFileRequest, FdOpenDirRequest, OpenDirResponse, OpenOptionsInternal,
    OpenRelativeFileRequest, ReadFileRequest, ReadLimitedFileRequest, SeekFileRequest,
    WriteFileRequest, WriteLimitedFileRequest, XstatFsRequest, XstatRequest,
};
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

pub(crate) mod filter;
pub(crate) mod hooks;
pub(crate) mod mapper;
pub(crate) mod open_dirs;
pub(crate) mod ops;

type RemoteFd = u64;
type LocalFd = RawFd;
type DirStreamFd = usize;

/// `OPEN_FILES` is used to track open files and their corrospending remote file descriptor.
/// We use Arc so we can support dup more nicely, this means that if user
/// Opens file `A`, receives fd 1, then dups, receives 2 - both stay open, until both are closed.
/// Previously in such scenario we would close the remote, causing issues.
pub(crate) static OPEN_FILES: LazyLock<Mutex<HashMap<LocalFd, Arc<ops::RemoteFile>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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
                        tracing::warn!("Invalid mode for fopen {:#?}", invalid);
                    }
                }

                open_options
            })
    }
}
