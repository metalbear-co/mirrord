#[cfg(target_os = "linux")]
use core::ffi::{c_size_t, c_ssize_t};
/// FFI functions that override the `libc` calls (see `file` module documentation on how to
/// enable/disable these).
///
/// NOTICE: If a file operation fails, it might be because it depends on some `libc` function
/// that is not being hooked (`strace` the program to check).
use std::{ffi::CString, os::unix::io::RawFd, ptr, slice, time::Duration};

use errno::{set_errno, Errno};
use libc::{
    self, c_char, c_int, c_void, dirent, iovec, off_t, size_t, ssize_t, stat, statfs, AT_EACCESS,
    AT_FDCWD, DIR, EINVAL, O_DIRECTORY, O_RDONLY,
};
#[cfg(target_os = "linux")]
use libc::{dirent64, stat64, statx, EBADF, ENOENT, ENOTDIR};
use mirrord_layer_macro::{hook_fn, hook_guard_fn};
use mirrord_protocol::file::{
    FsMetadataInternal, MetadataInternal, ReadFileResponse, WriteFileResponse,
};
#[cfg(target_os = "linux")]
use mirrord_protocol::ResponseError::{NotDirectory, NotFound};
use num_traits::Bounded;
use tracing::trace;
#[cfg(target_os = "linux")]
use tracing::{error, info, warn};

use super::{open_dirs, ops::*, OpenOptionsInternalExt};
#[cfg(target_os = "macos")]
use crate::detour::Bypass;
#[cfg(target_os = "linux")]
use crate::error::HookError::ResponseError;
use crate::{
    close_layer_fd,
    common::CheckedInto,
    detour::{Detour, DetourGuard},
    error::HookError,
    file::{
        open_dirs::OPEN_DIRS,
        ops::{access, lseek, open, read, write},
    },
    hooks::HookManager,
    replace,
};

#[cfg(target_os = "macos")]
#[allow(non_camel_case_types)]
type stat64 = stat;

/// Take the original raw c_char pointer and a resulting bypass, and either the original pointer or
/// a different one according to the bypass.
/// We pass reference to bypass to make sure the bypass lives with the pointer.
#[cfg(target_os = "macos")]
fn update_ptr_from_bypass(ptr: *const c_char, bypass: Bypass) -> *const c_char {
    match bypass {
        // For some reason, the program is trying to carry out an operation on a path that is
        // inside mirrord's temp bin dir. The detour has returned us the original path of the file
        // (stripped mirrord's dir path), so now we carry out the operation locally, on the stripped
        // path.
        Bypass::FileOperationInMirrordBinTempDir(stripped_ptr) => stripped_ptr,
        _ => ptr,
    }
}

/// Implementation of open_detour, used in open_detour and openat_detour
/// We ignore mode in case we don't bypass the call.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
unsafe fn open_logic(raw_path: *const c_char, open_flags: c_int, _mode: c_int) -> Detour<RawFd> {
    let path = raw_path.checked_into();
    let open_options = OpenOptionsInternalExt::from_flags(open_flags);

    trace!("path {:#?} | open_options {:#?}", path, open_options);

    open(path, open_options)
}

/// Hook for `libc::open`.
///
/// **Bypassed** by `raw_path`s that match what's in the `generate_local_set` regex, see
/// [`super::filter`].
#[hook_fn]
pub(super) unsafe extern "C" fn open_detour(
    raw_path: *const c_char,
    open_flags: c_int,
    mut args: ...
) -> RawFd {
    let mode: c_int = args.arg();
    let guard = DetourGuard::new();
    if guard.is_none() {
        FN_OPEN(raw_path, open_flags, mode)
    } else {
        open_logic(raw_path, open_flags, mode).unwrap_or_bypass_with(|_bypass| {
            #[cfg(target_os = "macos")]
            let raw_path = update_ptr_from_bypass(raw_path, _bypass);
            FN_OPEN(raw_path, open_flags, mode)
        })
    }
}

/// Hook for `libc::open64`.
///
/// **Bypassed** by `raw_path`s that match what's in the `generate_local_set` regex, see
/// [`super::filter`].
#[hook_fn]
pub(super) unsafe extern "C" fn open64_detour(
    raw_path: *const c_char,
    open_flags: c_int,
    mut args: ...
) -> RawFd {
    let mode: c_int = args.arg();
    let guard = DetourGuard::new();
    if guard.is_none() {
        FN_OPEN64(raw_path, open_flags, mode)
    } else {
        open_logic(raw_path, open_flags, mode).unwrap_or_bypass_with(|_bypass| {
            #[cfg(target_os = "macos")]
            let raw_path = update_ptr_from_bypass(raw_path, _bypass);
            FN_OPEN64(raw_path, open_flags, mode)
        })
    }
}

/// Hook for `libc::open$NOCANCEL`.
#[hook_fn]
pub(super) unsafe extern "C" fn open_nocancel_detour(
    raw_path: *const c_char,
    open_flags: c_int,
    mut args: ...
) -> RawFd {
    let mode: c_int = args.arg();
    let guard = DetourGuard::new();
    if guard.is_none() {
        FN_OPEN_NOCANCEL(raw_path, open_flags, mode)
    } else {
        open_logic(raw_path, open_flags, mode).unwrap_or_bypass_with(|_bypass| {
            #[cfg(target_os = "macos")]
            let raw_path = update_ptr_from_bypass(raw_path, _bypass);
            FN_OPEN_NOCANCEL(raw_path, open_flags, mode)
        })
    }
}

/// Hook for [`libc::opendir`].
///
/// Opens the directory with `read` permission using the [`open_logic`] flow, then calls
/// [`fdopendir`] to convert the [`RawFd`] into a `*DIR` stream (which we treat as `usize`).
#[hook_guard_fn]
pub(super) unsafe extern "C" fn opendir_detour(raw_filename: *const c_char) -> usize {
    open_logic(raw_filename, O_RDONLY, O_DIRECTORY)
        .and_then(|fd| match fdopendir(fd) {
            Detour::Success(success) => Detour::Success(success),
            Detour::Bypass(bypass) => {
                // this shouldn't happen, but if it does we shouldn't leak fd
                close_layer_fd(fd);
                Detour::Bypass(bypass)
            }
            Detour::Error(fail) => {
                close_layer_fd(fd);
                Detour::Error(fail)
            }
        })
        .unwrap_or_bypass_with(|_| opendir_bypass(raw_filename))
}

/// see below, to have nice code we also implement it for other archs.
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
unsafe fn opendir_bypass(raw_filename: *const c_char) -> usize {
    FN_OPENDIR(raw_filename)
}

/// on macOS aarch, for some reason when hooking it it crashes with illegal instruction on bypass
/// so we implement our own bypass
/// inspired by https://github.com/apple-oss-distributions/Libc/blob/c5a3293354e22262702a3add5b2dfc9bb0b93b85/gen/FreeBSD/opendir.c#L118
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
unsafe fn opendir_bypass(raw_filename: *const c_char) -> usize {
    let fd = libc::open(raw_filename, O_RDONLY | O_DIRECTORY);
    if fd == -1 {
        // null
        return 0;
    }

    let dir = libc::fdopendir(fd);
    if dir.is_null() {
        let errno = errno::errno();
        libc::close(fd);
        set_errno(errno);
        return 0;
    }
    dir as usize
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn fdopendir_detour(fd: RawFd) -> usize {
    fdopendir(fd).unwrap_or_bypass_with(|_| FN_FDOPENDIR(fd))
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn readdir_r_detour(
    dirp: *mut DIR,
    entry: *mut dirent,
    result: *mut *mut dirent,
) -> c_int {
    let Some(entry_ref) = entry.as_mut() else {
        return EINVAL;
    };

    OPEN_DIRS
        .read_r(dirp as usize)
        .map(|resp| {
            if let Some(direntry) = resp {
                match open_dirs::assign_direntry(direntry, entry_ref, false) {
                    Err(e) => return c_int::from(e),
                    Ok(()) => {
                        *result = entry;
                    }
                }
            } else {
                {
                    *result = std::ptr::null_mut();
                }
            }
            0
        })
        .unwrap_or_bypass_with(|_| FN_READDIR_R(dirp, entry, result))
}

#[cfg(target_os = "linux")]
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn readdir64_r_detour(
    dirp: *mut DIR,
    entry: *mut dirent64,
    result: *mut *mut dirent64,
) -> c_int {
    let Some(entry_ref) = entry.as_mut() else {
        return EINVAL;
    };

    OPEN_DIRS
        .read_r(dirp as usize)
        .map(|resp| {
            if let Some(direntry) = resp {
                match open_dirs::assign_direntry64(direntry, entry_ref, false) {
                    Err(e) => return c_int::from(e),
                    Ok(()) => {
                        *result = entry;
                    }
                }
            } else {
                {
                    *result = std::ptr::null_mut();
                }
            }
            0
        })
        .unwrap_or_bypass_with(|_| FN_READDIR64_R(dirp, entry, result))
}

#[cfg(target_os = "linux")]
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn readdir64_detour(dirp: *mut DIR) -> usize {
    match OPEN_DIRS.read64(dirp as usize) {
        Detour::Success(entry) => entry as usize,
        Detour::Bypass(..) => FN_READDIR64(dirp),
        Detour::Error(e) => {
            set_errno(Errno(e.into()));
            std::ptr::null::<dirent64>() as usize
        }
    }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn readdir_detour(dirp: *mut DIR) -> usize {
    match OPEN_DIRS.read(dirp as usize) {
        Detour::Success(entry) => entry as usize,
        Detour::Bypass(..) => FN_READDIR(dirp),
        Detour::Error(e) => {
            set_errno(Errno(e.into()));
            std::ptr::null::<dirent>() as usize
        }
    }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn closedir_detour(dirp: *mut DIR) -> c_int {
    OPEN_DIRS
        .close(dirp as usize)
        .unwrap_or_bypass_with(|_| FN_CLOSEDIR(dirp))
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn dirfd_detour(dirp: *mut DIR) -> c_int {
    OPEN_DIRS
        .get_fd(dirp as usize)
        .unwrap_or_bypass_with(|_| FN_DIRFD(dirp))
}

/// Equivalent to `open_detour`, **except** when `raw_path` specifies a relative path.
///
/// If `fd == AT_FDCWD`, the current working directory is used, and the behavior is the same as
/// `open_detour`.
/// `fd` for a file descriptor with the `O_DIRECTORY` flag.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn openat_detour(
    fd: RawFd,
    raw_path: *const c_char,
    open_flags: c_int,
) -> RawFd {
    let open_options = OpenOptionsInternalExt::from_flags(open_flags);

    openat(fd, raw_path.checked_into(), open_options).unwrap_or_bypass_with(|_bypass| {
        #[cfg(target_os = "macos")]
        let raw_path = update_ptr_from_bypass(raw_path, _bypass);
        FN_OPENAT(fd, raw_path, open_flags)
    })
}

/// Equivalent to `open_detour`, **except** when `raw_path` specifies a relative path.
///
/// If `fd == AT_FDCWD`, the current working directory is used, and the behavior is the same as
/// `open_detour`.
/// `fd` for a file descriptor with the `O_DIRECTORY` flag.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn openat64_detour(
    fd: RawFd,
    raw_path: *const c_char,
    open_flags: c_int,
) -> RawFd {
    let open_options = OpenOptionsInternalExt::from_flags(open_flags);

    openat(fd, raw_path.checked_into(), open_options).unwrap_or_bypass_with(|_bypass| {
        #[cfg(target_os = "macos")]
        let raw_path = update_ptr_from_bypass(raw_path, _bypass);
        FN_OPENAT64(fd, raw_path, open_flags)
    })
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn _openat_nocancel_detour(
    fd: RawFd,
    raw_path: *const c_char,
    open_flags: c_int,
) -> RawFd {
    let open_options = OpenOptionsInternalExt::from_flags(open_flags);

    openat(fd, raw_path.checked_into(), open_options).unwrap_or_bypass_with(|_bypass| {
        #[cfg(target_os = "macos")]
        let raw_path = update_ptr_from_bypass(raw_path, _bypass);
        FN__OPENAT_NOCANCEL(fd, raw_path, open_flags)
    })
}

/// Hook for getdents64, for Go's `os.ReadDir` on Linux.
#[cfg(target_os = "linux")]
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn getdents64_detour(
    fd: RawFd,
    dirent_buf: *mut c_void,
    buf_size: c_size_t,
) -> c_ssize_t {
    match getdents64(fd, buf_size as u64) {
        Detour::Success(res) => {
            let mut next = dirent_buf as *mut dirent;
            let end = next.byte_add(buf_size);
            for dent in res.entries {
                if next.byte_add(dent.get_d_reclen64() as usize) > end {
                    error!("Remote result for getdents64 would overflow local buffer.");
                    set_errno(Errno(EINVAL));
                    return -1;
                }

                let Some(next_ref) = next.as_mut() else {
                    set_errno(Errno(EINVAL));
                    return -1;
                };

                match open_dirs::assign_direntry(dent, next_ref, true) {
                    Err(e) => {
                        error!(
                            "Error while trying to write remote dir entry to local buffer: {e:?}"
                        );
                        // There is no appropriate error code for "We hijacked this operation and
                        // had an error while trying to create a CString."
                        set_errno(Errno(EBADF)); // Invalid file descriptor.
                        return -1;
                    }
                    Ok(()) => next = next.byte_add((*next).d_reclen as usize),
                }
            }
            res.result_size as c_ssize_t
        }
        Detour::Bypass(_) => {
            trace!("bypassing getdents64: calling syscall locally (fd: {fd}).");
            libc::syscall(libc::SYS_getdents64, fd, dirent_buf, buf_size) as c_ssize_t
        }
        Detour::Error(ResponseError(NotFound(not_found_fd))) => {
            info!(
                "Go application tried to read a directory and mirrord carried out that read on the \
                remote destination, however that directory was not found over there (local fd: \
                {fd}, remote fd: {not_found_fd})."
            );
            set_errno(Errno(ENOENT)); // "No such directory."
            -1
        }
        Detour::Error(ResponseError(NotDirectory(file_fd))) => {
            warn!(
                "Go application tried to read a directory and mirrord carried out that read on the \
                remote destination, however the type of that file on the remote destination is not \
                a directory (local fd: {fd}, remote fd: {file_fd})."
            );
            set_errno(Errno(ENOTDIR)); // "No such directory."
            -1
        }
        Detour::Error(err) => {
            error!("Encountered error in getdents64 detour: {err:?}");
            // There is no appropriate error code for "We hijacked this operation to a remote agent
            // and the agent returned an error". We could try to map more (remote) errors to
            // the error codes though.
            set_errno(Errno(EBADF));
            -1
        }
    }
}

/// Hook for `libc::read`.
///
/// Reads `count` bytes into `out_buffer`, only for `fd`s that are being managed by mirrord-layer.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn read_detour(
    fd: RawFd,
    out_buffer: *mut c_void,
    count: size_t,
) -> ssize_t {
    read(fd, count as u64)
        .map(|read_file| {
            let ReadFileResponse { bytes, read_amount } = read_file;

            // There is no distinction between reading 0 bytes or if we hit EOF, but we only copy to
            // buffer if we have something to copy.
            if read_amount > 0 {
                let read_ptr = bytes.as_ptr();
                let out_buffer = out_buffer.cast();
                ptr::copy(read_ptr, out_buffer, read_amount as usize);
            }

            // WARN: Must be careful when it comes to `EOF`, incorrect handling may appear as the
            // `read` call being repeated.
            ssize_t::try_from(read_amount).unwrap()
        })
        .unwrap_or_bypass_with(|_| FN_READ(fd, out_buffer, count))
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn _read_nocancel_detour(
    fd: RawFd,
    out_buffer: *mut c_void,
    count: size_t,
) -> ssize_t {
    read(fd, count as u64)
        .map(|read_file| {
            let ReadFileResponse { bytes, read_amount } = read_file;

            // There is no distinction between reading 0 bytes or if we hit EOF, but we only copy to
            // buffer if we have something to copy.
            if read_amount > 0 {
                let read_ptr = bytes.as_ptr();
                let out_buffer = out_buffer.cast();
                ptr::copy(read_ptr, out_buffer, read_amount as usize);
            }

            // WARN: Must be careful when it comes to `EOF`, incorrect handling may appear as the
            // `read` call being repeated.
            ssize_t::try_from(read_amount).unwrap()
        })
        .unwrap_or_bypass_with(|_| FN__READ_NOCANCEL(fd, out_buffer, count))
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn read_nocancel_detour(
    fd: RawFd,
    out_buffer: *mut c_void,
    count: size_t,
) -> ssize_t {
    read(fd, count as u64)
        .map(|read_file| {
            let ReadFileResponse { bytes, read_amount } = read_file;

            // There is no distinction between reading 0 bytes or if we hit EOF, but we only copy to
            // buffer if we have something to copy.
            if read_amount > 0 {
                let read_ptr = bytes.as_ptr();
                let out_buffer = out_buffer.cast();
                ptr::copy(read_ptr, out_buffer, read_amount as usize);
            }

            // WARN: Must be careful when it comes to `EOF`, incorrect handling may appear as the
            // `read` call being repeated.
            ssize_t::try_from(read_amount).unwrap()
        })
        .unwrap_or_bypass_with(|_| FN_READ_NOCANCEL(fd, out_buffer, count))
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn pread_detour(
    fd: RawFd,
    out_buffer: *mut c_void,
    amount_to_read: size_t,
    offset: off_t,
) -> ssize_t {
    pread(fd, amount_to_read as u64, offset as u64)
        .map(|read_file| {
            let ReadFileResponse { bytes, read_amount } = read_file;
            let fixed_read = (amount_to_read as u64).min(read_amount);

            // There is no distinction between reading 0 bytes or if we hit EOF, but we only
            // copy to buffer if we have something to copy.
            //
            // Callers can check for EOF by using `ferror`.
            if read_amount > 0 {
                let bytes_slice = bytes
                    .get(..fixed_read as usize)
                    .expect("read_amount exceeds bytes length in ReadFileResponse");

                ptr::copy(bytes_slice.as_ptr().cast(), out_buffer, bytes_slice.len());
            }
            fixed_read as ssize_t
        })
        .unwrap_or_bypass_with(|_| FN_PREAD(fd, out_buffer, amount_to_read, offset))
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn _pread_nocancel_detour(
    fd: RawFd,
    out_buffer: *mut c_void,
    amount_to_read: size_t,
    offset: off_t,
) -> ssize_t {
    pread(fd, amount_to_read as u64, offset as u64)
        .map(|read_file| {
            let ReadFileResponse { bytes, read_amount } = read_file;
            let fixed_read = (amount_to_read as u64).min(read_amount);

            // There is no distinction between reading 0 bytes or if we hit EOF, but we only
            // copy to buffer if we have something to copy.
            //
            // Callers can check for EOF by using `ferror`.
            if read_amount > 0 {
                let bytes_slice = bytes
                    .get(..fixed_read as usize)
                    .expect("read_amount exceeds bytes length in ReadFileResponse");

                ptr::copy(bytes_slice.as_ptr().cast(), out_buffer, bytes_slice.len());
            }
            fixed_read as ssize_t
        })
        .unwrap_or_bypass_with(|_| FN__PREAD_NOCANCEL(fd, out_buffer, amount_to_read, offset))
}

/// Common code between the `pwrite` detours.
///
/// Handle the `.unwrap_or_bypass` in their respective functions though.
unsafe fn pwrite_logic(
    fd: RawFd,
    in_buffer: *const c_void,
    amount_to_write: size_t,
    offset: off_t,
) -> Detour<ssize_t> {
    // Convert the given buffer into a u8 slice, upto the amount to write.
    let casted_in_buffer: &[u8] = slice::from_raw_parts(in_buffer.cast(), amount_to_write);

    pwrite(fd, casted_in_buffer, offset as u64).map(|write_response| {
        let WriteFileResponse { written_amount } = write_response;
        written_amount as ssize_t
    })
}

/// ## Note on go hook for this
///
/// On linux, this is `pwrite64`, but if you try to hook it and call as `FN_PWRITE64`, you won't
/// find it, resulting in an `unwrap on None` error when the detour is called. So, even though
/// golang receives a syscall of [`libc::SYS_pwrite64`], hooking it like this is what works.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn pwrite_detour(
    fd: RawFd,
    in_buffer: *const c_void,
    amount_to_write: size_t,
    offset: off_t,
) -> ssize_t {
    pwrite_logic(fd, in_buffer, amount_to_write, offset)
        .unwrap_or_bypass_with(|_| FN_PWRITE(fd, in_buffer, amount_to_write, offset))
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn _pwrite_nocancel_detour(
    fd: RawFd,
    in_buffer: *const c_void,
    amount_to_write: size_t,
    offset: off_t,
) -> ssize_t {
    pwrite_logic(fd, in_buffer, amount_to_write, offset)
        .unwrap_or_bypass_with(|_| FN__PWRITE_NOCANCEL(fd, in_buffer, amount_to_write, offset))
}

/// Hook for `libc::lseek`.
///
/// **Bypassed** by `fd`s that are not managed by us (not found in `OPEN_FILES`).
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn lseek_detour(fd: RawFd, offset: off_t, whence: c_int) -> off_t {
    lseek(fd, offset, whence)
        .map(|offset| i64::try_from(offset).unwrap())
        .unwrap_or_bypass_with(|_| FN_LSEEK(fd, offset, whence))
}

/// Implementation of write_detour, used in  write_detour
pub(crate) unsafe extern "C" fn write_logic(
    fd: RawFd,
    buffer: *const c_void,
    count: size_t,
) -> ssize_t {
    // WARN: Be veeery careful here, you cannot construct the `Vec` directly, as the buffer
    // allocation is handled on the C side.
    let write_bytes =
        (!buffer.is_null()).then(|| slice::from_raw_parts(buffer as *const u8, count).to_vec());

    write(fd, write_bytes).unwrap_or_bypass_with(|_| FN_WRITE(fd, buffer, count))
}

/// Hook for `libc::write`.
///
/// **Bypassed** by `fd`s that are not managed by us (not found in `OPEN_FILES`).
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn write_detour(
    fd: RawFd,
    buffer: *const c_void,
    count: size_t,
) -> ssize_t {
    write_logic(fd, buffer, count)
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn _write_nocancel_detour(
    fd: RawFd,
    buffer: *const c_void,
    count: size_t,
) -> ssize_t {
    // WARN: Be veeery careful here, you cannot construct the `Vec` directly, as the buffer
    // allocation is handled on the C side.
    let write_bytes =
        (!buffer.is_null()).then(|| slice::from_raw_parts(buffer as *const u8, count).to_vec());

    write(fd, write_bytes).unwrap_or_bypass_with(|_| FN__WRITE_NOCANCEL(fd, buffer, count))
}

/// Implementation of access_detour, used in access_detour and faccessat_detour
unsafe fn access_logic(raw_path: *const c_char, mode: c_int) -> c_int {
    access(raw_path.checked_into(), mode as u8).unwrap_or_bypass_with(|_bypass| {
        #[cfg(target_os = "macos")]
        let raw_path = update_ptr_from_bypass(raw_path, _bypass);
        FN_ACCESS(raw_path, mode)
    })
}

/// Hook for `libc::access`.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn access_detour(raw_path: *const c_char, mode: c_int) -> c_int {
    access_logic(raw_path, mode)
}

/// Hook for `libc::faccessat`.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn faccessat_detour(
    dirfd: RawFd,
    pathname: *const c_char,
    mode: c_int,
    flags: c_int,
) -> c_int {
    if dirfd == AT_FDCWD && (flags == AT_EACCESS || flags == 0) {
        access_logic(pathname, mode)
    } else {
        FN_FACCESSAT(dirfd, pathname, mode, flags)
    }
}

/// Hook for `libc::fsync`.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn fsync_detour(fd: RawFd) -> c_int {
    fsync(fd).unwrap_or_bypass_with(|_| FN_FSYNC(fd))
}

/// Hook for `libc::fdatasync`.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn fdatasync_detour(fd: RawFd) -> c_int {
    fsync(fd).unwrap_or_bypass_with(|_| FN_FDATASYNC(fd))
}

/// Tries to convert input to type O, if it fails it returns the max value of O.
/// For example, if you put u32::MAX into a u8, it will return u8::MAX.
fn best_effort_cast<I: Bounded, O: TryFrom<I> + Bounded>(input: I) -> O {
    input.try_into().unwrap_or_else(|_| O::max_value())
}

/// Converts time in nano seconds to seconds, to match the `stat` struct
/// which has very weird types used
fn nano_to_secs(nano: i64) -> i64 {
    best_effort_cast(Duration::from_nanos(best_effort_cast(nano)).as_secs())
}

/// Fills the `stat` struct with the metadata
unsafe extern "C" fn fill_stat(out_stat: *mut stat64, metadata: &MetadataInternal) {
    out_stat.write_bytes(0, 1);
    let out = &mut *out_stat;
    // on macOS the types might be different, so we try to cast and do our best..
    out.st_mode = best_effort_cast(metadata.mode);
    out.st_size = best_effort_cast(metadata.size);
    out.st_atime_nsec = metadata.access_time;
    out.st_mtime_nsec = metadata.modification_time;
    out.st_ctime_nsec = metadata.creation_time;
    out.st_atime = nano_to_secs(metadata.access_time);
    out.st_mtime = nano_to_secs(metadata.modification_time);
    out.st_ctime = nano_to_secs(metadata.creation_time);
    out.st_nlink = best_effort_cast(metadata.hard_links);
    out.st_uid = metadata.user_id;
    out.st_gid = metadata.group_id;
    out.st_dev = best_effort_cast(metadata.device_id);
    out.st_ino = best_effort_cast(metadata.inode);
    out.st_rdev = best_effort_cast(metadata.rdevice_id);
    out.st_blksize = best_effort_cast(metadata.block_size);
    out.st_blocks = best_effort_cast(metadata.blocks);
}

/// Fills the `statfs` struct with the metadata
unsafe extern "C" fn fill_statfs(out_stat: *mut statfs, metadata: &FsMetadataInternal) {
    // Acording to linux documentation "Fields that are undefined for a particular file system are
    // set to 0."
    out_stat.write_bytes(0, 1);
    let out = &mut *out_stat;
    // on macOS the types might be different, so we try to cast and do our best..
    out.f_type = best_effort_cast(metadata.filesystem_type);
    out.f_bsize = best_effort_cast(metadata.block_size);
    out.f_blocks = metadata.blocks;
    out.f_bfree = metadata.blocks_free;
    out.f_bavail = metadata.blocks_available;
    out.f_files = metadata.files;
    out.f_ffree = metadata.files_free;
}

fn stat_logic<const FOLLOW_SYMLINK: bool>(
    _ver: c_int,
    fd: Option<RawFd>,
    raw_path: Option<*const c_char>,
    out_stat: *mut stat64,
) -> Detour<c_int> {
    if out_stat.is_null() {
        Detour::Error(HookError::BadPointer)
    } else {
        let path = raw_path.map(CheckedInto::checked_into);

        xstat(path, fd, FOLLOW_SYMLINK).map(|res| {
            let res = res.metadata;
            unsafe { fill_stat(out_stat, &res) };
            0
        })
    }
}

/// Hook for `libc::lstat`.
#[hook_guard_fn]
unsafe extern "C" fn lstat_detour(raw_path: *const c_char, out_stat: *mut stat) -> c_int {
    stat_logic::<false>(0, None, Some(raw_path), out_stat as *mut _).unwrap_or_bypass_with(
        |_bypass| {
            #[cfg(target_os = "macos")]
            let raw_path = update_ptr_from_bypass(raw_path, _bypass);
            FN_LSTAT(raw_path, out_stat)
        },
    )
}

/// Hook for `libc::fstat`.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn fstat_detour(fd: RawFd, out_stat: *mut stat) -> c_int {
    stat_logic::<true>(0, Some(fd), None, out_stat as *mut _)
        .unwrap_or_bypass_with(|_| FN_FSTAT(fd, out_stat))
}

/// Hook for `libc::stat`.
#[hook_guard_fn]
unsafe extern "C" fn stat_detour(raw_path: *const c_char, out_stat: *mut stat) -> c_int {
    stat_logic::<true>(0, None, Some(raw_path), out_stat as *mut _).unwrap_or_bypass_with(
        |_bypass| {
            #[cfg(target_os = "macos")]
            let raw_path = update_ptr_from_bypass(raw_path, _bypass);
            FN_STAT(raw_path, out_stat)
        },
    )
}

/// Hook for `libc::statx`.
#[cfg(target_os = "linux")]
#[hook_guard_fn]
unsafe extern "C" fn statx_detour(
    dir_fd: RawFd,
    path_name: *const c_char,
    flags: c_int,
    mask: c_int,
    statx_buf: *mut statx,
) -> c_int {
    statx_logic(dir_fd, path_name, flags, mask, statx_buf)
        .unwrap_or_bypass_with(|_bypass| FN_STATX(dir_fd, path_name, flags, mask, statx_buf))
}

/// Hook for libc's stat syscall wrapper.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn __xstat_detour(
    ver: c_int,
    raw_path: *const c_char,
    out_stat: *mut stat,
) -> c_int {
    stat_logic::<true>(ver, None, Some(raw_path), out_stat as *mut _).unwrap_or_bypass_with(
        |_bypass| {
            #[cfg(target_os = "macos")]
            let raw_path = update_ptr_from_bypass(raw_path, _bypass);
            FN___XSTAT(ver, raw_path, out_stat)
        },
    )
}

/// Hook for libc's stat syscall wrapper.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn __lxstat_detour(
    ver: c_int,
    raw_path: *const c_char,
    out_stat: *mut stat,
) -> c_int {
    stat_logic::<true>(ver, None, Some(raw_path), out_stat as *mut _).unwrap_or_bypass_with(
        |_bypass| {
            #[cfg(target_os = "macos")]
            let raw_path = update_ptr_from_bypass(raw_path, _bypass);
            FN___LXSTAT(ver, raw_path, out_stat)
        },
    )
}

/// Hook for libc's stat syscall wrapper.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn __xstat64_detour(
    ver: c_int,
    raw_path: *const c_char,
    out_stat: *mut stat64,
) -> c_int {
    stat_logic::<true>(ver, None, Some(raw_path), out_stat).unwrap_or_bypass_with(|_bypass| {
        #[cfg(target_os = "macos")]
        let raw_path = update_ptr_from_bypass(raw_path, _bypass);
        FN___XSTAT64(ver, raw_path, out_stat)
    })
}

/// Hook for libc's stat syscall wrapper.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn __lxstat64_detour(
    ver: c_int,
    raw_path: *const c_char,
    out_stat: *mut stat64,
) -> c_int {
    stat_logic::<true>(ver, None, Some(raw_path), out_stat).unwrap_or_bypass_with(|_bypass| {
        #[cfg(target_os = "macos")]
        let raw_path = update_ptr_from_bypass(raw_path, _bypass);
        FN___LXSTAT64(ver, raw_path, out_stat)
    })
}

/// Separated out logic for `fstatat` so that it can be used by go to match on the xstat result.
pub(crate) unsafe fn fstatat_logic(
    fd: RawFd,
    raw_path: *const c_char,
    out_stat: *mut stat,
    flag: c_int,
) -> Detour<i32> {
    if out_stat.is_null() {
        return Detour::Error(HookError::BadPointer);
    }

    let follow_symlink = (flag & libc::AT_SYMLINK_NOFOLLOW) == 0;
    xstat(Some(raw_path.checked_into()), Some(fd), follow_symlink).map(|res| {
        let res = res.metadata;
        fill_stat(out_stat as *mut _, &res);
        0
    })
}

/// Hook for `libc::fstatat`.
#[hook_guard_fn]
unsafe extern "C" fn fstatat_detour(
    fd: RawFd,
    raw_path: *const c_char,
    out_stat: *mut stat,
    flag: c_int,
) -> c_int {
    fstatat_logic(fd, raw_path, out_stat, flag).unwrap_or_bypass_with(|_bypass| {
        #[cfg(target_os = "macos")]
        let raw_path = update_ptr_from_bypass(raw_path, _bypass);
        FN_FSTATAT(fd, raw_path, out_stat, flag)
    })
}

#[hook_guard_fn]
unsafe extern "C" fn fstatfs_detour(fd: c_int, out_stat: *mut statfs) -> c_int {
    if out_stat.is_null() {
        return HookError::BadPointer.into();
    }

    xstatfs(fd)
        .map(|res| {
            let res = res.metadata;
            fill_statfs(out_stat, &res);
            0
        })
        .unwrap_or_bypass_with(|_| FN_FSTATFS(fd, out_stat))
}

unsafe fn realpath_logic(
    source_path: *const c_char,
    output_path: *mut c_char,
) -> Detour<*mut c_char> {
    let path = source_path.checked_into();

    realpath(path).map(|res| {
        let path = CString::new(res.to_string_lossy().to_string()).unwrap();
        let path_len = path.as_bytes_with_nul().len();
        let output = if output_path.is_null() {
            let res = libc::malloc(path_len) as *mut c_char;
            if res.is_null() {
                return std::ptr::null_mut();
            }
            res
        } else {
            output_path
        };

        output
            .copy_from_nonoverlapping(path.as_ptr(), usize::min(libc::PATH_MAX as usize, path_len));
        output
    })
}

/// When path is handled by us, just make it absolute and return, since resolving it remotely
/// doesn't really matter for our case atm (might be in the future)
#[hook_guard_fn]
unsafe extern "C" fn realpath_detour(
    source_path: *const c_char,
    output_path: *mut c_char,
) -> *mut c_char {
    realpath_logic(source_path, output_path)
        .unwrap_or_bypass_with(|_| FN_REALPATH(source_path, output_path))
}

#[hook_guard_fn]
unsafe extern "C" fn realpath_darwin_extsn_detour(
    source_path: *const c_char,
    output_path: *mut c_char,
) -> *mut c_char {
    realpath_logic(source_path, output_path)
        .unwrap_or_bypass_with(|_| FN_REALPATH_DARWIN_EXTSN(source_path, output_path))
}

fn vec_to_iovec(bytes: &[u8], iovecs: &[iovec]) {
    let mut copied = 0;
    let mut iov_index = 0;

    while copied < bytes.len() {
        let iov = &iovecs.get(iov_index).expect("ioevec out of bounds");
        let read_ptr = unsafe { bytes.as_ptr().add(copied) };
        let copy_amount = std::cmp::min(bytes.len(), iov.iov_len);
        let out_buffer = iov.iov_base.cast();
        unsafe { ptr::copy(read_ptr, out_buffer, copy_amount) };
        copied += copy_amount;
        // we trust iov_index to be in correct size since we checked it before
        iov_index += 1;
    }
}

/// Hook for `libc::readv`.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn readv_detour(
    fd: RawFd,
    iovecs: *const iovec,
    iovec_count: c_int,
) -> ssize_t {
    if iovec_count < 0 {
        return FN_READV(fd, iovecs, iovec_count);
    }

    let iovs = (!iovecs.is_null()).then(|| slice::from_raw_parts(iovecs, iovec_count as usize));

    readv(iovs)
        .and_then(|(iovs, read_size)| Detour::Success((read(fd, read_size)?, iovs)))
        .map(|(read_file, iovs)| {
            let ReadFileResponse { bytes, .. } = read_file;

            vec_to_iovec(&bytes, iovs);
            // WARN: Must be careful when it comes to `EOF`, incorrect handling may appear as the
            // `read` call being repeated.
            ssize_t::try_from(bytes.len()).unwrap()
        })
        .unwrap_or_bypass_with(|_| FN_READV(fd, iovecs, iovec_count))
}

/// Hook for `libc::readv`.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn preadv_detour(
    fd: RawFd,
    iovecs: *const iovec,
    iovec_count: c_int,
    offset: off_t,
) -> ssize_t {
    if iovec_count < 0 {
        return FN_PREADV(fd, iovecs, iovec_count, offset);
    }

    let iovs = (!iovecs.is_null()).then(|| slice::from_raw_parts(iovecs, iovec_count as usize));

    readv(iovs)
        .and_then(|(iovs, read_size)| Detour::Success((pread(fd, read_size, offset as u64)?, iovs)))
        .map(|(read_file, iovs)| {
            let ReadFileResponse { bytes, .. } = read_file;

            vec_to_iovec(&bytes, iovs);

            // WARN: Must be careful when it comes to `EOF`, incorrect handling may appear as the
            // `read` call being repeated.
            ssize_t::try_from(bytes.len()).unwrap()
        })
        .unwrap_or_bypass_with(|_| FN_PREADV(fd, iovecs, iovec_count, offset))
}

/// Convenience function to setup file hooks (`x_detour`) with `frida_gum`.
pub(crate) unsafe fn enable_file_hooks(hook_manager: &mut HookManager) {
    replace!(hook_manager, "open", open_detour, FnOpen, FN_OPEN);
    replace!(hook_manager, "open64", open64_detour, FnOpen64, FN_OPEN64);
    replace!(
        hook_manager,
        "open$NOCANCEL",
        open_nocancel_detour,
        FnOpen_nocancel,
        FN_OPEN_NOCANCEL
    );

    replace!(hook_manager, "openat", openat_detour, FnOpenat, FN_OPENAT);
    replace!(
        hook_manager,
        "openat64",
        openat64_detour,
        FnOpenat64,
        FN_OPENAT64
    );
    replace!(
        hook_manager,
        "_openat$NOCANCEL",
        _openat_nocancel_detour,
        Fn_openat_nocancel,
        FN__OPENAT_NOCANCEL
    );

    replace!(hook_manager, "read", read_detour, FnRead, FN_READ);
    replace!(
        hook_manager,
        "_read$NOCANCEL",
        _read_nocancel_detour,
        Fn_read_nocancel,
        FN__READ_NOCANCEL
    );

    replace!(
        hook_manager,
        "read$NOCANCEL",
        read_nocancel_detour,
        FnRead_nocancel,
        FN_READ_NOCANCEL
    );

    replace!(
        hook_manager,
        "closedir",
        closedir_detour,
        FnClosedir,
        FN_CLOSEDIR
    );

    replace!(hook_manager, "dirfd", dirfd_detour, FnDirfd, FN_DIRFD);

    replace!(hook_manager, "pread", pread_detour, FnPread, FN_PREAD);
    replace!(hook_manager, "readv", readv_detour, FnReadv, FN_READV);
    replace!(hook_manager, "preadv", preadv_detour, FnPreadv, FN_PREADV);
    replace!(
        hook_manager,
        "_pread$NOCANCEL",
        _pread_nocancel_detour,
        Fn_pread_nocancel,
        FN__PREAD_NOCANCEL
    );

    replace!(hook_manager, "lseek", lseek_detour, FnLseek, FN_LSEEK);

    replace!(hook_manager, "write", write_detour, FnWrite, FN_WRITE);
    replace!(
        hook_manager,
        "_write$NOCANCEL",
        _write_nocancel_detour,
        Fn_write_nocancel,
        FN__WRITE_NOCANCEL
    );

    replace!(hook_manager, "pwrite", pwrite_detour, FnPwrite, FN_PWRITE);
    replace!(
        hook_manager,
        "_pwrite$NOCANCEL",
        _pwrite_nocancel_detour,
        Fn_pwrite_nocancel,
        FN__PWRITE_NOCANCEL
    );

    replace!(hook_manager, "access", access_detour, FnAccess, FN_ACCESS);
    replace!(
        hook_manager,
        "faccessat",
        faccessat_detour,
        FnFaccessat,
        FN_FACCESSAT
    );

    replace!(hook_manager, "fsync", fsync_detour, FnFsync, FN_FSYNC);
    replace!(
        hook_manager,
        "fdatasync",
        fdatasync_detour,
        FnFdatasync,
        FN_FDATASYNC
    );

    replace!(
        hook_manager,
        "realpath",
        realpath_detour,
        FnRealpath,
        FN_REALPATH
    );

    replace!(
        hook_manager,
        "realpath$DARWIN_EXTSN",
        realpath_darwin_extsn_detour,
        FnRealpath_darwin_extsn,
        FN_REALPATH_DARWIN_EXTSN
    );

    #[cfg(target_os = "linux")]
    {
        replace!(hook_manager, "statx", statx_detour, FnStatx, FN_STATX);
    }

    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    {
        replace!(
            hook_manager,
            "__xstat",
            __xstat_detour,
            Fn__xstat,
            FN___XSTAT
        );
        replace!(
            hook_manager,
            "__xstat64",
            __xstat64_detour,
            Fn__xstat64,
            FN___XSTAT64
        );
        replace!(
            hook_manager,
            "__lxstat",
            __lxstat_detour,
            Fn__lxstat,
            FN___LXSTAT
        );
        replace!(
            hook_manager,
            "__lxstat64",
            __lxstat64_detour,
            Fn__lxstat64,
            FN___LXSTAT64
        );
        replace!(hook_manager, "lstat", lstat_detour, FnLstat, FN_LSTAT);
        crate::replace_with_fallback!(
            hook_manager,
            "fstat",
            fstat_detour,
            FnFstat,
            FN_FSTAT,
            libc::fstat
        );
        replace!(hook_manager, "stat", stat_detour, FnStat, FN_STAT);
        replace!(
            hook_manager,
            "fstatat",
            fstatat_detour,
            FnFstatat,
            FN_FSTATAT
        );

        replace!(
            hook_manager,
            "fstatfs",
            fstatfs_detour,
            FnFstatfs,
            FN_FSTATFS
        );
        replace!(
            hook_manager,
            "fdopendir",
            fdopendir_detour,
            FnFdopendir,
            FN_FDOPENDIR
        );
        replace!(
            hook_manager,
            "readdir_r",
            readdir_r_detour,
            FnReaddir_r,
            FN_READDIR_R
        );
        #[cfg(target_os = "linux")]
        replace!(
            hook_manager,
            "readdir64_r",
            readdir64_r_detour,
            FnReaddir64_r,
            FN_READDIR64_R
        );
        #[cfg(target_os = "linux")]
        replace!(
            hook_manager,
            "readdir64",
            readdir64_detour,
            FnReaddir64,
            FN_READDIR64
        );
        replace!(
            hook_manager,
            "readdir",
            readdir_detour,
            FnReaddir,
            FN_READDIR
        );
        // aarch + macOS hooks fail
        // because macOs internally calls this with pointer authentication
        // and we don't compile to arm64e yet, so it breaks.
        // but it seems we'll be able to compile to arm64e soon.
        // https://github.com/rust-lang/rust/pull/115526
        replace!(
            hook_manager,
            "opendir",
            opendir_detour,
            FnOpendir,
            FN_OPENDIR
        );
    }
    // on non aarch64 (Intel) we need to hook also $INODE64 variants
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        replace!(
            hook_manager,
            "lstat$INODE64",
            lstat_detour,
            FnLstat,
            FN_LSTAT
        );
        replace!(
            hook_manager,
            "fstat$INODE64",
            fstat_detour,
            FnFstat,
            FN_FSTAT
        );
        replace!(hook_manager, "stat$INODE64", stat_detour, FnStat, FN_STAT);
        replace!(
            hook_manager,
            "fstatat$INODE64",
            fstatat_detour,
            FnFstatat,
            FN_FSTATAT
        );
        replace!(
            hook_manager,
            "fstatfs$INODE64",
            fstatfs_detour,
            FnFstatfs,
            FN_FSTATFS
        );
        replace!(
            hook_manager,
            "fdopendir$INODE64",
            fdopendir_detour,
            FnFdopendir,
            FN_FDOPENDIR
        );
        replace!(
            hook_manager,
            "readdir_r$INODE64",
            readdir_r_detour,
            FnReaddir_r,
            FN_READDIR_R
        );
        replace!(
            hook_manager,
            "readdir$INODE64",
            readdir_detour,
            FnReaddir,
            FN_READDIR
        );
        replace!(
            hook_manager,
            "opendir$INODE64",
            opendir_detour,
            FnOpendir,
            FN_OPENDIR
        );
    }
}
