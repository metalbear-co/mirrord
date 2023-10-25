//! Implementation of directory listing in the layer. Used in hooks like `opendir`, `closedir` or
//! `readdir` family.

use std::{
    ffi::CString,
    sync::{Arc, LazyLock, Mutex},
};

use dashmap::DashMap;
use mirrord_protocol::file::{CloseDirRequest, DirEntryInternal, ReadDirRequest, ReadDirResponse};

use super::{DirStreamFd, RemoteFd};
use crate::{
    common,
    detour::{Bypass, Detour},
    error::HookError,
};

/// Global instance of [`OpenDirs`]. Used in hooks.
pub static OPEN_DIRS: LazyLock<OpenDirs> = LazyLock::new(OpenDirs::new);

/// State related to open remote directories.
pub struct OpenDirs {
    inner: DashMap<DirStreamFd, Arc<Mutex<OpenDir>>>,
}

impl OpenDirs {
    /// Creates an empty state.
    fn new() -> Self {
        Self {
            inner: DashMap::with_capacity(4),
        }
    }

    /// Inserts a new entry into this state.
    ///
    /// # Arguments
    ///
    /// * `local_dir_fd` - opaque identifier
    /// * `remote_fd` - descriptor of the remote directory (received from the agent)
    pub fn insert(&self, local_dir_fd: DirStreamFd, remote_fd: RemoteFd) {
        self.inner.insert(
            local_dir_fd,
            Mutex::new(OpenDir::new(local_dir_fd, remote_fd)).into(),
        );
    }

    /// Reads next entry from the open directory with the given [`DirStreamFd`].
    pub fn read_r(&self, local_dir_fd: DirStreamFd) -> Detour<Option<DirEntryInternal>> {
        let dir = self
            .inner
            .get(&local_dir_fd)
            .ok_or(Bypass::LocalDirStreamNotFound(local_dir_fd))?
            .clone();

        let guard = dir.lock().expect("lock poisoned");

        guard.read_r()
    }

    /// Reads next entry from the open directory with the given [`DirStreamFd`] and copies the data
    /// into an internal buffer. Returns a raw pointer to that buffer.
    ///
    /// If there are no more entries, returns [`std::ptr::null`].
    ///
    /// # Notes
    ///
    /// 1. There is exactly one buffer of this type for each open directory.
    /// 2. The returned pointer is valid as long as the directory remains open.
    /// 3. Consecutive calls of this method overwrite the buffer.
    ///
    /// Given above, the returned pointer is good enough to be used as a result of `readdir`.
    pub fn read(&self, local_dir_fd: DirStreamFd) -> Detour<*const libc::dirent> {
        let dir = self
            .inner
            .get(&local_dir_fd)
            .ok_or(Bypass::LocalDirStreamNotFound(local_dir_fd))?
            .clone();

        let mut guard = dir.lock().expect("lock poisoned");

        let Some(entry) = guard.read_r()? else {
            return Detour::Success(std::ptr::null());
        };

        assign_direntry(entry, &mut guard.dirent, false)?;

        Detour::Success(&guard.dirent)
    }

    /// Reads next entry from the open directory with the given [`DirStreamFd`] and copies the data
    /// into an internal buffer. Returns a raw pointer to that buffer.
    ///
    /// If there are no more entries, returns [`std::ptr::null`].
    ///
    /// # Notes
    ///
    /// 1. There is exactly one buffer of this type for each open directory.
    /// 2. The returned pointer is valid as long as the directory remains open.
    /// 3. Consecutive calls of this method overwrite the buffer.
    ///
    /// Given above, the returned pointer is good enough to be used as a result of `readdir64`.
    #[cfg(target_os = "linux")]
    pub fn read64(&self, local_dir_fd: DirStreamFd) -> Detour<*const libc::dirent64> {
        let dir = self
            .inner
            .get(&local_dir_fd)
            .ok_or(Bypass::LocalDirStreamNotFound(local_dir_fd))?
            .clone();

        let mut guard = dir.lock().expect("lock poisoned");

        let Some(entry) = guard.read_r()? else {
            return Detour::Success(std::ptr::null());
        };

        assign_direntry64(entry, &mut guard.dirent64, false)?;

        Detour::Success(&guard.dirent64)
    }

    /// Closes the open directory with the given [`DirStreamFd`].
    pub fn close(&self, local_dir_fd: DirStreamFd) -> Detour<libc::c_int> {
        let (_, dir) = self
            .inner
            .remove(&local_dir_fd)
            .ok_or(Bypass::LocalDirStreamNotFound(local_dir_fd))?;

        let mut guard = dir.lock().expect("lock poisoned");
        guard.closed = true;

        common::make_proxy_request_no_response(CloseDirRequest {
            remote_fd: guard.remote_fd,
        })?;

        Detour::Success(0)
    }
}

struct OpenDir {
    closed: bool,
    local_fd: DirStreamFd,
    remote_fd: RemoteFd,
    dirent: libc::dirent,
    #[cfg(target_os = "linux")]
    dirent64: libc::dirent64,
}

impl OpenDir {
    fn new(local_fd: DirStreamFd, remote_fd: RemoteFd) -> Self {
        #[cfg(not(target_os = "macos"))]
        let dirent = libc::dirent {
            d_ino: 0,
            d_off: 0,
            d_reclen: 0,
            d_type: 0,
            d_name: [0; 256],
        };

        #[cfg(target_os = "macos")]
        let dirent = libc::dirent {
            d_ino: 0,
            d_reclen: 0,
            d_type: 0,
            d_name: [0; 1024],
            d_seekoff: 0,
            d_namlen: 0,
        };

        Self {
            closed: false,
            local_fd,
            remote_fd,
            dirent,
            #[cfg(target_os = "linux")]
            dirent64: libc::dirent64 {
                d_ino: 0,
                d_off: 0,
                d_reclen: 0,
                d_type: 0,
                d_name: [0; 256],
            },
        }
    }

    fn read_r(&self) -> Detour<Option<DirEntryInternal>> {
        if self.closed {
            // This thread got this struct from `OpenDirs` before `close` removed it.
            return Detour::Bypass(Bypass::LocalDirStreamNotFound(self.local_fd));
        }

        let ReadDirResponse { direntry } =
            common::make_proxy_request_with_response(ReadDirRequest {
                remote_fd: self.remote_fd,
            })??;
        Detour::Success(direntry)
    }
}

/// Moves [`DirEntryInternal`] content to the given [`libc::dirent`].
pub fn assign_direntry(
    in_entry: DirEntryInternal,
    out_entry: &mut libc::dirent,
    getdents: bool,
) -> Result<(), HookError> {
    out_entry.d_ino = in_entry.inode;
    out_entry.d_reclen = if getdents {
        // The structs written by the kernel for the getdents syscall do not have a fixed size.
        in_entry.get_d_reclen64()
    } else {
        std::mem::size_of::<libc::dirent>() as u16
    };
    out_entry.d_type = in_entry.file_type;

    let dir_name = CString::new(in_entry.name)?;
    let dir_name_bytes = dir_name.as_bytes_with_nul();
    out_entry
        .d_name
        .get_mut(..dir_name_bytes.len())
        .expect("directory name length exceeds limit")
        .copy_from_slice(bytemuck::cast_slice(dir_name_bytes));

    #[cfg(target_os = "macos")]
    {
        out_entry.d_seekoff = in_entry.position;
        // name length should be without null
        out_entry.d_namlen = dir_name.to_bytes().len() as u16;
    }

    #[cfg(target_os = "linux")]
    {
        out_entry.d_off = in_entry.position as i64;
    }
    Ok(())
}

/// Moves [`DirEntryInternal`] content to the given [`libc::dirent64`].
#[cfg(target_os = "linux")]
pub fn assign_direntry64(
    in_entry: DirEntryInternal,
    out_entry: &mut libc::dirent64,
    getdents: bool,
) -> Result<(), HookError> {
    out_entry.d_ino = in_entry.inode;
    out_entry.d_reclen = if getdents {
        // The structs written by the kernel for the getdents syscall do not have a fixed size.
        in_entry.get_d_reclen64()
    } else {
        std::mem::size_of::<libc::dirent64>() as u16
    };
    out_entry.d_type = in_entry.file_type;

    let dir_name = CString::new(in_entry.name)?;
    let dir_name_bytes = dir_name.as_bytes_with_nul();
    out_entry
        .d_name
        .get_mut(..dir_name_bytes.len())
        .expect("directory name length exceeds limit")
        .copy_from_slice(bytemuck::cast_slice(dir_name_bytes));

    out_entry.d_off = in_entry.position as i64;

    Ok(())
}
