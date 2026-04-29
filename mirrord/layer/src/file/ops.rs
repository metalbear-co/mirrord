//! # Note on path handling
//!
//! When operating on the paths provided from the user application, remember to verify/remap them.
//! Canonical order of operations can be found in [`common_path_check`].

#[cfg(target_os = "linux")]
use std::time::Duration;
use std::{
    env,
    ffi::CString,
    io::SeekFrom,
    os::unix::io::RawFd,
    path::{Path, PathBuf},
};

use libc::{AT_FDCWD, c_int, iovec};
#[cfg(target_os = "linux")]
use libc::{c_char, statx, statx_timestamp};
use mirrord_config::feature::fs::FsModeConfig;
use mirrord_layer_lib::{
    detour::{Bypass, Detour},
    error::{HookError, HookResult as Result},
    file::filter::FileFilter,
};
use mirrord_protocol::{
    Payload, ResponseError,
    file::{
        FchmodRequest, FchownRequest, FtruncateRequest, FutimensRequest, MakeDirAtRequest,
        MakeDirRequest, OpenFileRequest, OpenFileResponse, OpenOptionsInternal, ReadFileResponse,
        ReadLinkFileRequest, ReadLinkFileResponse, RemoveDirRequest, RenameRequest,
        SeekFileResponse, StatFsRequestV2, Timespec, UnlinkAtRequest, UnlinkRequest,
        WriteFileResponse, XstatFsRequestV2, XstatFsResponseV2, XstatResponse,
    },
};
use nix::errno::Errno;
use rand::distr::{Alphanumeric, SampleString};
#[cfg(debug_assertions)]
use tracing::Level;
use tracing::{error, trace};

use super::{hooks::FN_OPEN, open_dirs::OPEN_DIRS, *};
use crate::common;
#[cfg(target_os = "linux")]
use crate::common::CheckedInto;

/// 1 Megabyte. Large read requests can lead to timeouts.
const MAX_READ_SIZE: u64 = 1024 * 1024;

/// Convenience extension for verifying that a [`Path`] is not relative.
trait PathExt {
    /// If this [`Path`] is relative and is not present in the `fs.not_found` filters, returns a
    /// [`Detour::Bypass`], otherwise errors with [`HookError::FileNotFound`].
    fn ensure_not_relative_or_not_found(&self) -> Detour<()>;
}

impl PathExt for Path {
    fn ensure_not_relative_or_not_found(&self) -> Detour<()> {
        if self.is_relative() {
            if crate::setup().file_filter().check_not_found(self) {
                Detour::Error(HookError::FileNotFound(self.to_string_lossy().to_string()))
            } else {
                Detour::Bypass(Bypass::relative_path(self.to_str().unwrap_or_default()))
            }
        } else {
            Detour::Success(())
        }
    }
}

/// Checks whether the given [`Path`] should be accessed remotely.
pub fn ensure_remote(file_filter: &FileFilter, path: &Path, write: bool) -> Detour<()> {
    // TODO(gabriela): rewrite this using `FileFilter::check`!

    let text = path.to_str().unwrap_or_default();

    match file_filter.mode {
        FsModeConfig::Local => Detour::Bypass(Bypass::ignored_file(text)),
        _ if file_filter.not_found.is_match(text) => {
            Detour::Error(HookError::FileNotFound(text.to_string()))
        }
        _ if file_filter.read_write.is_match(text) => Detour::Success(()),
        _ if file_filter.read_only.is_match(text) => {
            if write {
                Detour::Bypass(Bypass::ignored_file(text))
            } else {
                Detour::Success(())
            }
        }
        _ if file_filter.local.is_match(text) => Detour::Bypass(Bypass::ignored_file(text)),
        _ if file_filter.default_not_found.is_match(text) => {
            Detour::Error(HookError::FileNotFound(text.to_string()))
        }
        _ if file_filter.default_remote_ro.is_match(text) && !write => Detour::Success(()),
        _ if file_filter.default_local.is_match(text) => Detour::Bypass(Bypass::ignored_file(text)),
        FsModeConfig::LocalWithOverrides => Detour::Bypass(Bypass::ignored_file(text)),
        FsModeConfig::Write => Detour::Success(()),
        FsModeConfig::Read if write => Detour::Bypass(Bypass::ReadOnly(text.into())),
        FsModeConfig::Read => Detour::Success(()),
    }
}

/// Performs standard verification of paths accessed by the user application.
///
/// Operations in order:
/// 1. Bypass if the path is not relative and not present in the `fs.not_found` filters.
/// 2. Remap the file according to the config.
/// 3. Bypass if the new path should be accessed locally.
///
/// Returns the remapped path.
fn common_path_check(path: PathBuf, write: bool) -> Detour<PathBuf> {
    path.ensure_not_relative_or_not_found()?;

    let path = crate::setup().file_remapper().change_path(path);
    ensure_remote(crate::setup().file_filter(), &path, write)?;
    Detour::Success(path)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RemoteFile {
    pub fd: u64,
    pub path: String,
}

impl RemoteFile {
    pub(crate) fn new(fd: u64, path: String) -> Self {
        Self { fd, path }
    }

    /// Sends a [`OpenFileRequest`] message, opening the file in the agent.
    #[mirrord_layer_macro::instrument(level = "trace")]
    pub(crate) fn remote_open(
        path: PathBuf,
        open_options: OpenOptionsInternal,
    ) -> Detour<OpenFileResponse> {
        let requesting_file = OpenFileRequest { path, open_options };

        let response = common::make_proxy_request_with_response(requesting_file)??;

        Detour::Success(response)
    }

    /// Sends a [`ReadFileRequest`] message, reading the file in the agent.
    ///
    /// Blocking request and wait on already found remote_fd
    #[mirrord_layer_macro::instrument(level = "trace")]
    pub(crate) fn remote_read(remote_fd: u64, read_amount: u64) -> Detour<ReadFileResponse> {
        // Limit read size because if we read too much it can lead to a timeout
        // Seems also that bincode doesn't do well with large buffers
        let read_amount = std::cmp::min(read_amount, MAX_READ_SIZE);
        let reading_file = ReadFileRequest {
            remote_fd,
            buffer_size: read_amount,
        };

        let response = common::make_proxy_request_with_response(reading_file)??;

        Detour::Success(response)
    }

    /// Sends a [`CloseFileRequest`] message, closing the file in the agent.
    #[mirrord_layer_macro::instrument(level = "trace")]
    pub(crate) fn remote_close(fd: u64) -> Result<()> {
        common::make_proxy_request_no_response(CloseFileRequest { fd })?;
        Ok(())
    }
}

impl Drop for RemoteFile {
    fn drop(&mut self) {
        // Warning: Don't log from here. This is called when self is removed from OPEN_FILES, so
        // during the whole execution of this function, OPEN_FILES is locked.
        // When emitting logs, sometimes a file `write` operation is required, in order for the
        // operation to complete. The write operation is hooked and at some point tries to lock
        // `OPEN_FILES`, which means the thread deadlocks with itself (we call
        // `OPEN_FILES.lock()?.remove()` and then while still locked, `OPEN_FILES.lock()` again)
        let result = Self::remote_close(self.fd);
        assert!(
            result.is_ok(),
            "mirrord failed to send close file message to main layer thread. Error: {result:?}",
        );
    }
}

/// Helper function that retrieves the `remote_fd` (which is generated by
/// `mirrord_agent::util::IndexAllocator`).
fn get_remote_fd(local_fd: RawFd) -> Detour<u64> {
    // don't add a trace here since it causes deadlocks in some cases.
    Detour::Success(
        OPEN_FILES
            .lock()?
            .get(&local_fd)
            .map(|remote_file| remote_file.fd)
            // Bypass if we're not managing the relative part.
            .ok_or(Bypass::LocalFdNotFound(local_fd))?,
    )
}

/// Create temporary local file to get a valid local fd.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
fn create_local_fake_file(remote_fd: u64) -> Detour<RawFd> {
    if crate::setup().experimental().use_dev_null {
        return create_local_devnull_file(remote_fd);
    }
    let random_string = Alphanumeric.sample_string(&mut rand::rng(), 16);
    let file_name = format!("{remote_fd}-{random_string}");
    let file_path = env::temp_dir().join(file_name);
    let file_c_string = CString::new(file_path.to_string_lossy().to_string())?;
    let file_path_ptr = file_c_string.as_ptr();
    let local_file_fd: RawFd = unsafe { FN_OPEN(file_path_ptr, O_RDONLY | O_CREAT) };
    if local_file_fd == -1 {
        let error = Errno::last_raw();
        // Close the remote file if creating a tmp local file failed and we have an invalid local fd
        close_remote_file_on_failure(remote_fd)?;
        Detour::Error(HookError::LocalFileCreation(remote_fd, error))
    } else {
        unsafe { libc::unlink(file_path_ptr) };
        Detour::Success(local_file_fd)
    }
}

/// Open /dev/null to get a valid file fd
#[mirrord_layer_macro::instrument(level = "trace", ret)]
fn create_local_devnull_file(remote_fd: u64) -> Detour<RawFd> {
    let file_c_string = CString::new("/dev/null")?;
    let file_path_ptr = file_c_string.as_ptr();
    let local_file_fd: RawFd = unsafe { FN_OPEN(file_path_ptr, O_RDONLY) };
    if local_file_fd == -1 {
        let error = Errno::last_raw();
        // Close the remote file if creating a tmp local file failed and we have an invalid local fd
        close_remote_file_on_failure(remote_fd)?;
        Detour::Error(HookError::LocalFileCreation(remote_fd, error))
    } else {
        Detour::Success(local_file_fd)
    }
}

/// Close the remote file if the call to [`libc::shm_open`] failed and we have an invalid local fd.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
fn close_remote_file_on_failure(fd: u64) -> Result<()> {
    error!("Creating a temporary local file resulted in an error, closing the file remotely!");
    RemoteFile::remote_close(fd)
}

/// Blocking wrapper around `libc::open` call.
///
/// **Bypassed** when trying to load system files, and files from the current working directory
/// (which is different anyways when running in `-agent` context).
///
/// When called for a valid file, it blocks and sends an open file request to be handled by
/// `mirrord-agent`, and waits until it receives an open file response.
///
/// [`open`] is also used by other _open-ish_ functions, and it takes care of **creating** the
/// _local_ and _remote_ file association, plus **inserting** it into the storage for
/// [`OPEN_FILES`].
#[mirrord_layer_macro::instrument(level = Level::TRACE, ret)]
pub(crate) fn open(path: Detour<PathBuf>, open_options: OpenOptionsInternal) -> Detour<RawFd> {
    let path = common_path_check(path?, open_options.is_write())?;

    let OpenFileResponse { fd: remote_fd } = RemoteFile::remote_open(path.clone(), open_options)
        .or_else(|fail| match fail {
            // The operator has a policy that matches this `path` as local-only.
            HookError::ResponseError(ResponseError::OpenLocal) => Detour::Bypass(Bypass::OpenLocal),
            other => Detour::Error(other),
        })?;

    // TODO: Need a way to say "open a directory", right now `is_dir` always returns false.
    // This requires having a fake directory name (`/fake`, for example), instead of just converting
    // the fd to a string.
    let local_file_fd = create_local_fake_file(remote_fd)?;

    OPEN_FILES.lock()?.insert(
        local_file_fd,
        Arc::new(RemoteFile::new(remote_fd, path.display().to_string())),
    );

    Detour::Success(local_file_fd)
}

/// creates a directory stream for the `remote_fd` in the agent
#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub(crate) fn fdopendir(fd: RawFd) -> Detour<usize> {
    // usize == ptr size
    // we don't return a pointer to an address that contains DIR
    let remote_file_fd = OPEN_FILES
        .lock()?
        .get(&fd)
        .ok_or(Bypass::LocalFdNotFound(fd))?
        .fd;

    let open_dir_request = FdOpenDirRequest {
        remote_fd: remote_file_fd,
    };

    let OpenDirResponse { fd: remote_dir_fd } =
        common::make_proxy_request_with_response(open_dir_request)??;

    let local_dir_fd = create_local_fake_file(remote_dir_fd)?;
    OPEN_DIRS.insert(local_dir_fd as usize, remote_dir_fd, fd)?;

    // Let it stay in OPEN_FILES, as some functions might use it in comibination with dirfd

    Detour::Success(local_dir_fd as usize)
}

#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub(crate) fn openat(
    fd: RawFd,
    path: Detour<PathBuf>,
    open_options: OpenOptionsInternal,
) -> Detour<RawFd> {
    let path = path?;

    // `openat` behaves the same as `open` when the path is absolute. When called with AT_FDCWD, the
    // call is propagated to `open`.
    if path.is_absolute() || fd == AT_FDCWD {
        return open(Detour::Success(path), open_options);
    }

    // Relative path requires special handling, we must identify the relative part
    // (relative to what).
    let remote_fd = get_remote_fd(fd)?;

    let requesting_file = OpenRelativeFileRequest {
        relative_fd: remote_fd,
        path: path.clone(),
        open_options,
    };

    let OpenFileResponse { fd: remote_fd } =
        common::make_proxy_request_with_response(requesting_file)??;

    let local_file_fd = create_local_fake_file(remote_fd)?;

    OPEN_FILES.lock()?.insert(
        local_file_fd,
        Arc::new(RemoteFile::new(remote_fd, path.display().to_string())),
    );

    Detour::Success(local_file_fd)
}

/// Blocking wrapper around [`libc::read`] call.
///
/// **Bypassed** when trying to load system files, and files from the current working directory, see
/// `open`.
pub(crate) fn read(local_fd: RawFd, read_amount: u64) -> Detour<ReadFileResponse> {
    get_remote_fd(local_fd).and_then(|remote_fd| RemoteFile::remote_read(remote_fd, read_amount))
}

/// Helper for dealing with a potential null pointer being passed to `*const iovec` from
/// `readv_detour` and `preadv_detour`.
pub(crate) fn readv(iovs: Option<&[iovec]>) -> Detour<(&[iovec], u64)> {
    let iovs = iovs?;
    let read_size: u64 = iovs.iter().fold(0, |sum, iov| sum + iov.iov_len as u64);

    Detour::Success((iovs, read_size))
}

#[mirrord_layer_macro::instrument(level = "trace")]
pub(crate) fn pread(local_fd: RawFd, buffer_size: u64, offset: u64) -> Detour<ReadFileResponse> {
    // We're only interested in files that are paired with mirrord-agent.
    let remote_fd = get_remote_fd(local_fd)?;

    let reading_file = ReadLimitedFileRequest {
        remote_fd,
        buffer_size,
        start_from: offset,
    };

    let response = common::make_proxy_request_with_response(reading_file)??;

    Detour::Success(response)
}

/// Resolves the symbolic link `path`.
#[mirrord_layer_macro::instrument(level = Level::TRACE, ret)]
pub(crate) fn read_link(path: Detour<PathBuf>) -> Detour<ReadLinkFileResponse> {
    let path = common_path_check(path?, false)?;

    let requesting_path = ReadLinkFileRequest { path };

    // `NotImplemented` error here means that the protocol doesn't support it.
    match common::make_proxy_request_with_response(requesting_path)? {
        Ok(response) => Detour::Success(response),
        Err(ResponseError::NotImplemented) => Detour::Bypass(Bypass::NotImplemented),
        Err(fail) => Detour::Error(fail.into()),
    }
}

#[mirrord_layer_macro::instrument(level = Level::TRACE, ret)]
pub(crate) fn mkdir(path: Detour<PathBuf>, mode: u32) -> Detour<()> {
    let path = common_path_check(path?, true)?;

    let mkdir = MakeDirRequest {
        pathname: path,
        mode,
    };

    // `NotImplemented` error here means that the protocol doesn't support it.
    match common::make_proxy_request_with_response(mkdir)? {
        Ok(response) => Detour::Success(response),
        Err(ResponseError::NotImplemented) => Detour::Bypass(Bypass::NotImplemented),
        Err(fail) => Detour::Error(fail.into()),
    }
}

#[mirrord_layer_macro::instrument(level = Level::TRACE, ret)]
pub(crate) fn mkdirat(dirfd: RawFd, path: Detour<PathBuf>, mode: u32) -> Detour<()> {
    let path = path?;

    if path.is_absolute() || dirfd == AT_FDCWD {
        return mkdir(Detour::Success(path), mode);
    }

    // Relative path requires special handling, we must identify the relative part (relative to
    // what).
    let remote_fd = get_remote_fd(dirfd)?;

    let mkdir: MakeDirAtRequest = MakeDirAtRequest {
        dirfd: remote_fd,
        pathname: path.clone(),
        mode,
    };

    // `NotImplemented` error here means that the protocol doesn't support it.
    match common::make_proxy_request_with_response(mkdir)? {
        Ok(response) => Detour::Success(response),
        Err(ResponseError::NotImplemented) => Detour::Bypass(Bypass::NotImplemented),
        Err(fail) => Detour::Error(fail.into()),
    }
}

#[mirrord_layer_macro::instrument(level = Level::TRACE, ret)]
pub(crate) fn rmdir(path: Detour<PathBuf>) -> Detour<()> {
    let path = common_path_check(path?, true)?;

    let rmdir = RemoveDirRequest { pathname: path };

    // `NotImplemented` error here means that the protocol doesn't support it.
    match common::make_proxy_request_with_response(rmdir)? {
        Ok(response) => Detour::Success(response),
        Err(ResponseError::NotImplemented) => Detour::Bypass(Bypass::NotImplemented),
        Err(fail) => Detour::Error(fail.into()),
    }
}

#[mirrord_layer_macro::instrument(level = Level::TRACE, ret)]
pub(crate) fn unlink(path: Detour<PathBuf>) -> Detour<()> {
    let path = common_path_check(path?, true)?;

    let unlink = UnlinkRequest { pathname: path };

    // `NotImplemented` error here means that the protocol doesn't support it.
    match common::make_proxy_request_with_response(unlink)? {
        Ok(response) => Detour::Success(response),
        Err(ResponseError::NotImplemented) => Detour::Bypass(Bypass::NotImplemented),
        Err(fail) => Detour::Error(fail.into()),
    }
}

#[mirrord_layer_macro::instrument(level = Level::TRACE, ret)]
pub(crate) fn unlinkat(dirfd: RawFd, path: Detour<PathBuf>, flags: u32) -> Detour<()> {
    let mut path = path?;

    if dirfd == AT_FDCWD {
        path.ensure_not_relative_or_not_found()?;
    }

    if path.is_absolute() {
        path = crate::setup().file_remapper().change_path(path);
        ensure_remote(crate::setup().file_filter(), &path, true)?;
    }

    let unlink = if path.is_absolute() || dirfd == AT_FDCWD {
        UnlinkAtRequest {
            dirfd: None,
            pathname: path,
            flags,
        }
    } else {
        let remote_fd = get_remote_fd(dirfd)?;

        UnlinkAtRequest {
            dirfd: Some(remote_fd),
            pathname: path,
            flags,
        }
    };

    // `NotImplemented` error here means that the protocol doesn't support it.
    match common::make_proxy_request_with_response(unlink)? {
        Ok(response) => Detour::Success(response),
        Err(ResponseError::NotImplemented) => Detour::Bypass(Bypass::NotImplemented),
        Err(fail) => Detour::Error(fail.into()),
    }
}

pub(crate) fn pwrite(local_fd: RawFd, buffer: &[u8], offset: u64) -> Detour<WriteFileResponse> {
    let remote_fd = get_remote_fd(local_fd)?;
    trace!("pwrite: local_fd {local_fd}");
    let write_bytes = Payload::from(buffer.to_vec());
    let writing_file = WriteLimitedFileRequest {
        remote_fd,
        write_bytes,
        start_from: offset,
    };

    let response = common::make_proxy_request_with_response(writing_file)??;

    Detour::Success(response)
}

#[mirrord_layer_macro::instrument(level = "trace")]
pub(crate) fn lseek(local_fd: RawFd, offset: i64, whence: i32) -> Detour<u64> {
    let remote_fd = get_remote_fd(local_fd)?;

    let seek_from = match whence {
        libc::SEEK_SET => SeekFrom::Start(offset as u64),
        libc::SEEK_CUR => SeekFrom::Current(offset),
        libc::SEEK_END => SeekFrom::End(offset),
        invalid => {
            tracing::warn!(
                "lseek -> potential invalid value {:#?} for whence {:#?}",
                invalid,
                whence
            );
            return Detour::Bypass(Bypass::CStrConversion);
        }
    };

    let seeking_file = SeekFileRequest {
        fd: remote_fd,
        seek_from: seek_from.into(),
    };

    let SeekFileResponse { result_offset } =
        common::make_proxy_request_with_response(seeking_file)??;

    Detour::Success(result_offset)
}

pub(crate) fn write(local_fd: RawFd, write_bytes: Option<Vec<u8>>) -> Detour<isize> {
    let remote_fd = get_remote_fd(local_fd)?;

    let writing_file = WriteFileRequest {
        fd: remote_fd,
        write_bytes: write_bytes.ok_or(Bypass::EmptyBuffer)?.into(),
    };

    let WriteFileResponse { written_amount } =
        common::make_proxy_request_with_response(writing_file)??;
    Detour::Success(written_amount.try_into()?)
}

#[mirrord_layer_macro::instrument(level = "trace")]
pub(crate) fn access(path: Detour<PathBuf>, mode: c_int) -> Detour<c_int> {
    // Even though `access` is never a write operation (even if mode is write), we take the mode
    // into account when deciding whether to ignore, because when a caller is asking whether they
    // have write access to a file and then write to it, we want the test and the actual write to
    // happen with the same file.
    let path = common_path_check(path?, (mode & libc::W_OK) != 0)?;

    let access = AccessFileRequest {
        pathname: path,
        mode: mode as u8,
    };

    let _ = common::make_proxy_request_with_response(access)??;

    Detour::Success(0)
}

/// Original function _flushes_ data from `fd` to disk, but we don't really do any of this
/// for our managed fds, so we just return `0` which means success.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub(crate) fn fsync(fd: RawFd) -> Detour<c_int> {
    get_remote_fd(fd)?;
    Detour::Success(0)
}

/// General stat function that can be used for lstat, fstat, stat and fstatat.
///
/// Note: We treat cases of `AT_SYMLINK_NOFOLLOW_ANY` as `AT_SYMLINK_NOFOLLOW` because even Go does
/// that.
///
/// `rawish_path` is `Option<Detour<PathBuf>>` because we need to differentiate between null pointer
/// and non existing argument (for error handling)
#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub(crate) fn xstat(
    rawish_path: Option<Detour<PathBuf>>,
    fd: Option<RawFd>,
    follow_symlink: bool,
) -> Detour<XstatResponse> {
    // Can't use map because we need to propagate captured error
    let (path, fd) = match (rawish_path, fd) {
        // fstatat
        (Some(path), Some(fd)) => {
            let mut path = path?;

            let fd = {
                if fd == AT_FDCWD {
                    path = common_path_check(path, false)?;
                    None
                } else if path.is_absolute() {
                    path = crate::setup().file_remapper().change_path(path);
                    ensure_remote(crate::setup().file_filter(), &path, true)?;
                    None
                } else {
                    Some(get_remote_fd(fd)?)
                }
            };

            (Some(path), fd)
        }

        // lstat/stat
        (Some(path), None) => {
            let path = common_path_check(path?, false)?;
            (Some(path), None)
        }

        // fstat
        (None, Some(fd)) => (None, Some(get_remote_fd(fd)?)),

        // can't happen
        (None, None) => return Detour::Error(HookError::NullPointer),
    };

    let xstat = XstatRequest {
        fd,
        path,
        follow_symlink,
    };

    let response = common::make_proxy_request_with_response(xstat)??;

    Detour::Success(response)
}

/// Logic for the `libc::statx` function.
/// See [manual](https://man7.org/linux/man-pages/man2/statx.2.html) for reference.
///
/// # Warning
///
/// Due to backwards compatibility on the [`mirrord_protocol`] level, we use [`XstatRequest`] to get
/// the remote file metadata.
/// Because of this, we're not able to fill all field of the [`struct@statx`] structure. Missing
/// fields are:
/// 1. [`statx::stx_attributes`]
/// 2. [`statx::stx_ctime`]
/// 3. [`statx::stx_mnt_id`]
/// 4. [`statx::stx_dio_mem_align`] and [`statx::stx_dio_offset_align`]
///
/// Luckily, [`statx::stx_mask`] and [`statx::stx_attributes_mask`] fields allow us to inform the
/// caller about respective fields being skipped.
#[cfg(target_os = "linux")]
pub(crate) fn statx_logic(
    dirfd: RawFd,
    path_name: *const c_char,
    flags: c_int,
    mask: c_int,
    statx_buf: *mut statx,
) -> Detour<c_int> {
    // SAFETY: we don't check pointers passed as arguments to hooked functions

    use std::{mem, ops::Not};

    use mirrord_layer_lib::detour::OptionDetourExt;

    let statx_buf = unsafe { statx_buf.as_mut().ok_or(HookError::BadPointer)? };

    if (mask & libc::STATX__RESERVED) != 0 {
        return Detour::Error(HookError::BadFlag);
    }

    let mut path: Option<PathBuf> = path_name
        .is_null()
        .not()
        .then(|| path_name.checked_into())
        .transpose()?;
    if path.is_none() && (flags & libc::AT_EMPTY_PATH) == 0 {
        return Detour::Error(HookError::BadPointer);
    }

    path = path.filter(|p| p.as_os_str().is_empty().not());
    if path.is_none() && (flags & libc::AT_EMPTY_PATH) == 0 {
        return Detour::Error(HookError::EmptyPath);
    }

    let fd = (dirfd != libc::AT_FDCWD).then_some(dirfd);

    let (fd, path) = match (fd, path) {
        (None, None) => return Detour::Bypass(Bypass::LocalFdNotFound(dirfd)),
        (Some(fd), None) => (Some(get_remote_fd(fd)?), None),
        (Some(fd), Some(path)) if path.is_relative() => {
            let fd = get_remote_fd(fd)?;
            (Some(fd), Some(path))
        }
        (_, Some(path)) => {
            let path = common_path_check(path, false)?;
            (None, Some(path))
        }
    };

    let response = {
        let fd = fd
            .map(u64::try_from)
            .transpose()
            .map_err(|_| HookError::BadDescriptor)?;
        let follow_symlink = (flags & libc::AT_SYMLINK_NOFOLLOW) == 0;

        let request = XstatRequest {
            fd,
            path,
            follow_symlink,
        };

        common::make_proxy_request_with_response(request)??.metadata
    };

    /// Converts a nanosecond timestamp from
    /// [`MetadataInternal`](mirrord_protocol::file::MetadataInternal) to [`statx_timestamp`]
    /// format.
    fn nanos_to_statx(nanos: i64) -> statx_timestamp {
        let duration = Duration::from_nanos(nanos.try_into().unwrap_or(0));

        // Safety: This is just initializing a C-type, we should be fine.
        let mut result = unsafe { mem::zeroed::<statx_timestamp>() };
        result.tv_sec = duration.as_secs().try_into().unwrap_or(i64::MAX);
        result.tv_nsec = duration.subsec_nanos();

        result
    }

    /// Converts a device id from [`MetadataInternal`](mirrord_protocol::file::MetadataInternal) to
    /// format expected by [`statx`]: (major,minor) number.
    fn device_id_to_statx(id: u64) -> (u32, u32) {
        (libc::major(id), libc::minor(id))
    }

    // SAFETY: all-zero statx struct is valid
    *statx_buf = unsafe { std::mem::zeroed() };
    statx_buf.stx_mask = libc::STATX_TYPE
        & libc::STATX_MODE
        & libc::STATX_NLINK
        & libc::STATX_UID
        & libc::STATX_GID
        & libc::STATX_ATIME
        & libc::STATX_MTIME
        & libc::STATX_CTIME
        & libc::STATX_INO
        & libc::STATX_SIZE
        & libc::STATX_BLOCKS;
    statx_buf.stx_attributes_mask = 0;

    statx_buf.stx_blksize = response.block_size.try_into().unwrap_or(u32::MAX);
    statx_buf.stx_nlink = response.hard_links.try_into().unwrap_or(u32::MAX);
    statx_buf.stx_uid = response.user_id;
    statx_buf.stx_gid = response.group_id;
    statx_buf.stx_mode = response.mode as u16; // we only care about the lower half
    statx_buf.stx_ino = response.inode;
    statx_buf.stx_size = response.size;
    statx_buf.stx_blocks = response.blocks;
    statx_buf.stx_atime = nanos_to_statx(response.access_time);
    statx_buf.stx_ctime = nanos_to_statx(response.creation_time);
    statx_buf.stx_mtime = nanos_to_statx(response.modification_time);
    let (major, minor) = device_id_to_statx(response.rdevice_id);
    statx_buf.stx_rdev_major = major;
    statx_buf.stx_rdev_minor = minor;
    let (major, minor) = device_id_to_statx(response.device_id);
    statx_buf.stx_dev_major = major;
    statx_buf.stx_dev_minor = minor;

    Detour::Success(0)
}

#[mirrord_layer_macro::instrument(level = "trace")]
pub(crate) fn xstatfs(fd: RawFd) -> Detour<XstatFsResponseV2> {
    let fd = get_remote_fd(fd)?;

    // intproxy downgrades to old version if new one is not supported by agent, and converts
    // old version responses to V2 responses.
    let xstatfs = XstatFsRequestV2 { fd };

    let response = common::make_proxy_request_with_response(xstatfs)??;

    Detour::Success(response)
}

/// Gets all the data for statfs64, but can be used also for statfs.
#[mirrord_layer_macro::instrument(level = "trace")]
pub(crate) fn statfs(path: Detour<PathBuf>) -> Detour<XstatFsResponseV2> {
    let path = common_path_check(path?, false)?;

    // intproxy downgrades to old version if new one is not supported by agent, and converts
    // old version responses to V2 responses.
    let statfs = StatFsRequestV2 { path };

    let response = common::make_proxy_request_with_response(statfs)??;

    Detour::Success(response)
}

#[cfg(target_os = "linux")]
#[mirrord_layer_macro::instrument(level = "trace")]
pub(crate) fn getdents64(fd: RawFd, buffer_size: u64) -> Detour<GetDEnts64Response> {
    // We're only interested in files that are paired with mirrord-agent.
    let remote_fd = get_remote_fd(fd)?;

    let getdents64 = GetDEnts64Request {
        remote_fd,
        buffer_size,
    };

    let response = common::make_proxy_request_with_response(getdents64)??;

    Detour::Success(response)
}

/// Resolves ./ and ../ in the path, and returns an absolute path.
fn absolute_path(path: PathBuf) -> PathBuf {
    use std::path::Component;
    let mut temp_path = PathBuf::new();
    temp_path.push("/");
    for c in path.components() {
        match c {
            Component::RootDir => {}
            Component::CurDir => {}
            Component::Normal(p) => temp_path.push(p),
            Component::ParentDir => {
                temp_path.pop();
            }
            Component::Prefix(_) => {}
        }
    }
    temp_path
}

#[mirrord_layer_macro::instrument(level = "trace")]
pub(crate) fn realpath(path: Detour<PathBuf>) -> Detour<PathBuf> {
    let path = common_path_check(path?, false)?;

    let realpath = absolute_path(path);

    // check that file exists
    xstat(Some(Detour::Success(realpath.clone())), None, true)?;

    Detour::Success(realpath)
}

/// Renames a file/dir from `old_path` to `new_path`, replacing the original.
///
/// - When `fs.mapping` config is being used, we need to remap both `old_path` and `new_path`, so we
///   cannot do the usual `common_path_check(...)?` on each path, as this would return only 1 of the
///   paths remapped.
#[mirrord_layer_macro::instrument(level = Level::TRACE, ret)]
pub(crate) fn rename(old_path: Detour<PathBuf>, new_path: Detour<PathBuf>) -> Detour<()> {
    let old_path = common_path_check(old_path?, false);
    let new_path = common_path_check(new_path?, false);

    // We need to remap both `old_path` and `new_path` on bypass.
    let (old_path, new_path) = match (old_path, new_path) {
        (Detour::Success(old_path), Detour::Success(new_path)) => {
            Detour::Success((old_path, new_path))
        }
        (
            Detour::Bypass(Bypass::IgnoredFile(old_path)),
            Detour::Bypass(Bypass::IgnoredFile(new_path)),
        ) => Detour::Bypass(Bypass::IgnoredFiles(Some(old_path), Some(new_path))),
        (Detour::Bypass(Bypass::IgnoredFile(old_path)), Detour::Success(..)) => {
            Detour::Bypass(Bypass::IgnoredFiles(Some(old_path), None))
        }
        (Detour::Success(..), Detour::Bypass(Bypass::IgnoredFile(new_path))) => {
            Detour::Bypass(Bypass::IgnoredFiles(None, Some(new_path)))
        }
        (old, new) => Detour::Success((old?, new?)),
    }?;

    let old_path = absolute_path(old_path);
    let new_path = absolute_path(new_path);

    Detour::Success(common::make_proxy_request_with_response(RenameRequest {
        old_path,
        new_path,
    })??)
}

pub(crate) fn ftruncate(fd: RawFd, length: i64) -> Detour<()> {
    let fd = get_remote_fd(fd)?;
    Detour::Success(common::make_proxy_request_with_response(
        FtruncateRequest { fd, length },
    )??)
}

pub(crate) fn futimens(fd: RawFd, times: Option<[Timespec; 2]>) -> Detour<()> {
    let fd = get_remote_fd(fd)?;
    Detour::Success(common::make_proxy_request_with_response(
        FutimensRequest { fd, times },
    )??)
}

pub(crate) fn fchown(fd: RawFd, owner: u32, group: u32) -> Detour<()> {
    let fd = get_remote_fd(fd)?;
    Detour::Success(common::make_proxy_request_with_response(FchownRequest {
        fd,
        owner,
        group,
    })??)
}

pub(crate) fn fchmod(fd: RawFd, mode: u32) -> Detour<()> {
    let fd = get_remote_fd(fd)?;
    Detour::Success(common::make_proxy_request_with_response(FchmodRequest {
        fd,
        mode,
    })??)
}

#[cfg(test)]
mod test {
    use std::path::PathBuf;

    use mirrord_config::{feature::fs::FsConfig, util::VecOrSingle};
    use mirrord_layer_lib::detour::Detour;
    use rstest::*;

    use super::{absolute_path, *};
    #[test]
    fn test_absolute_normal() {
        assert_eq!(
            absolute_path(PathBuf::from("/a/b/c")),
            PathBuf::from("/a/b/c")
        );
        assert_eq!(
            absolute_path(PathBuf::from("/a/b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            absolute_path(PathBuf::from("/a/b/./c")),
            PathBuf::from("/a/b/c")
        )
    }

    /// Helper type for testing [`FileFilter`] results.
    #[derive(PartialEq, Eq, Debug)]
    enum DetourKind {
        Bypass,
        Error,
        Success,
    }

    impl<S> From<&Detour<S>> for DetourKind {
        fn from(detour: &Detour<S>) -> Self {
            match detour {
                Detour::Bypass(..) => DetourKind::Bypass,
                Detour::Error(..) => DetourKind::Error,
                Detour::Success(..) => DetourKind::Success,
            }
        }
    }

    #[rstest]
    #[trace]
    #[case(FsModeConfig::Write, "/a/test.a", false, DetourKind::Success)]
    #[case(
        FsModeConfig::Write,
        "/pain/read_write/test.a",
        false,
        DetourKind::Success
    )]
    #[case(
        FsModeConfig::Write,
        "/pain/read_only/test.a",
        false,
        DetourKind::Success
    )]
    #[case(FsModeConfig::Write, "/pain/write.a", false, DetourKind::Success)]
    #[case(FsModeConfig::Write, "/pain/local/test.a", false, DetourKind::Bypass)]
    #[case(FsModeConfig::Write, "/opt/test.a", false, DetourKind::Bypass)]
    #[case(FsModeConfig::Write, "/a/test.a", true, DetourKind::Success)]
    #[case(
        FsModeConfig::Write,
        "/pain/read_write/test.a",
        true,
        DetourKind::Success
    )]
    #[case(
        FsModeConfig::Write,
        "/pain/read_only/test.a",
        true,
        DetourKind::Bypass
    )]
    #[case(FsModeConfig::Write, "/pain/write.a", true, DetourKind::Success)]
    #[case(FsModeConfig::Write, "/pain/local/test.a", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Write, "/opt/test.a", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Read, "/a/test.a", false, DetourKind::Success)]
    #[case(
        FsModeConfig::Read,
        "/pain/read_write/test.a",
        false,
        DetourKind::Success
    )]
    #[case(
        FsModeConfig::Read,
        "/pain/read_only/test.a",
        false,
        DetourKind::Success
    )]
    #[case(FsModeConfig::Read, "/pain/write.a", false, DetourKind::Success)]
    #[case(FsModeConfig::Read, "/pain/local/test.a", false, DetourKind::Bypass)]
    #[case(FsModeConfig::Read, "/opt/test.a", false, DetourKind::Bypass)]
    #[case(FsModeConfig::Read, "/a/test.a", true, DetourKind::Bypass)]
    #[case(
        FsModeConfig::Read,
        "/pain/read_write/test.a",
        true,
        DetourKind::Success
    )]
    #[case(FsModeConfig::Read, "/pain/read_only/test.a", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Read, "/pain/write.a", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Read, "/pain/local/test.a", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Read, "/opt/test.a", true, DetourKind::Bypass)]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/a/test.a",
        false,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/read_write/test.a",
        false,
        DetourKind::Success
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/read_only/test.a",
        false,
        DetourKind::Success
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/write.a",
        false,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/local/test.a",
        false,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/opt/test.a",
        false,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/a/test.a",
        true,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/read_write/test.a",
        true,
        DetourKind::Success
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/read_only/test.a",
        true,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/write.a",
        true,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/local/test.a",
        true,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/opt/test.a",
        true,
        DetourKind::Bypass
    )]
    #[case(FsModeConfig::Read, "/etc/resolv.conf", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Write, "/etc/resolv.conf", true, DetourKind::Bypass)]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/etc/resolv.conf",
        true,
        DetourKind::Bypass
    )]
    #[case(FsModeConfig::Local, "/a/test.a", false, DetourKind::Bypass)]
    #[case(
        FsModeConfig::Local,
        "/pain/read_write/test.a",
        false,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::Local,
        "/pain/read_only/test.a",
        false,
        DetourKind::Bypass
    )]
    #[case(FsModeConfig::Local, "/pain/write.a", false, DetourKind::Bypass)]
    #[case(FsModeConfig::Local, "/pain/local/test.a", false, DetourKind::Bypass)]
    #[case(FsModeConfig::Local, "/opt/test.a", false, DetourKind::Bypass)]
    #[case(FsModeConfig::Local, "/a/test.a", true, DetourKind::Bypass)]
    #[case(
        FsModeConfig::Local,
        "/pain/read_write/test.a",
        true,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::Local,
        "/pain/read_only/test.a",
        true,
        DetourKind::Bypass
    )]
    #[case(FsModeConfig::Local, "/pain/write.a", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Local, "/pain/local/test.a", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Local, "/opt/test.a", true, DetourKind::Bypass)]
    #[case(
        FsModeConfig::Local,
        "/pain/not_found/test.a",
        false,
        DetourKind::Bypass
    )]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/pain/not_found/test.a",
        false,
        DetourKind::Error
    )]
    #[case(FsModeConfig::Read, "/pain/not_found/test.a", false, DetourKind::Error)]
    #[case(
        FsModeConfig::Write,
        "/pain/not_found/test.a",
        false,
        DetourKind::Error
    )]
    fn include_complex_configuration(
        #[case] mode: FsModeConfig,
        #[case] path: &str,
        #[case] write: bool,
        #[case] expected: DetourKind,
    ) {
        use mirrord_config::feature::fs::READONLY_FILE_BUFFER_DEFAULT;

        let read_write = Some(VecOrSingle::Multiple(vec![
            r"/pain/read_write.*\.a".to_string(),
        ]));
        let read_only = Some(VecOrSingle::Multiple(vec![
            r"/pain/read_only.*\.a".to_string(),
        ]));
        let local = Some(VecOrSingle::Multiple(vec![r"/pain/local.*\.a".to_string()]));
        let not_found = Some(VecOrSingle::Single(r"/pain/not_found.*\.a".to_string()));
        let fs_config = FsConfig {
            read_write,
            read_only,
            local,
            not_found,
            mode,
            mapping: None,
            readonly_file_buffer: READONLY_FILE_BUFFER_DEFAULT,
        };

        let file_filter = FileFilter::new(fs_config);

        let res = ensure_remote(&file_filter, Path::new(path), write);
        println!("filter result: {res:?}");
        assert_eq!(DetourKind::from(&res), expected);
    }

    #[rstest]
    #[case(FsModeConfig::Read, "/etc/resolv.conf", true, DetourKind::Bypass)]
    #[case(FsModeConfig::Write, "/etc/resolv.conf", true, DetourKind::Bypass)]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/etc/resolv.conf",
        true,
        DetourKind::Bypass
    )]
    #[case(FsModeConfig::Read, "/etc/resolv.conf", false, DetourKind::Success)]
    #[case(FsModeConfig::Write, "/etc/resolv.conf", false, DetourKind::Success)]
    #[case(
        FsModeConfig::LocalWithOverrides,
        "/etc/resolv.conf",
        false,
        DetourKind::Success
    )]
    fn remote_read_only_set(
        #[case] mode: FsModeConfig,
        #[case] path: &str,
        #[case] write: bool,
        #[case] expected: DetourKind,
    ) {
        use mirrord_config::feature::fs::READONLY_FILE_BUFFER_DEFAULT;

        let fs_config = FsConfig {
            mode,
            readonly_file_buffer: READONLY_FILE_BUFFER_DEFAULT,
            ..Default::default()
        };

        let file_filter = FileFilter::new(fs_config);

        let res = ensure_remote(&file_filter, Path::new(path), write);
        println!("filter result: {res:?}");

        assert_eq!(DetourKind::from(&res), expected);
    }

    /// Sanity test for empty [`RegexSet`] behaviour.
    #[test]
    fn empty_regex_set() {
        let set = FileFilter::make_regex_set(None).unwrap();
        assert!(!set.is_match("/path/to/some/file"));
    }

    /// Return path to the $HOME directory without trailing slash.
    fn clean_home() -> String {
        env::var("HOME").unwrap().trim_end_matches('/').into()
    }

    #[rstest]
    #[case(&format!("{}/.config/gcloud/some_file", clean_home()), DetourKind::Error)]
    #[case("/root/.config/gcloud/some_file", DetourKind::Success)]
    #[case("/root/.nuget/packages/microsoft.azure.amqp", DetourKind::Success)]
    fn not_found_set(#[case] path: &str, #[case] expected: DetourKind) {
        let filter = FileFilter::new(Default::default());
        let res = ensure_remote(&filter, Path::new(path), false);
        println!("filter result: {res:?}");

        assert_eq!(DetourKind::from(&res), expected);
    }
}
