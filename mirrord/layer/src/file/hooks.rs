use core::ffi::{c_size_t, c_ssize_t};
/// FFI functions that override the `libc` calls (see `file` module documentation on how to
/// enable/disable these).
///
/// NOTICE: If a file operation fails, it might be because it depends on some `libc` function
/// that is not being hooked (`strace` the program to check).
use std::{
    ffi::{CStr, CString},
    os::unix::io::RawFd,
    ptr, slice,
    time::Duration,
};

use errno::{set_errno, Errno};
use libc::{
    self, c_char, c_int, c_void, dirent, off_t, size_t, ssize_t, stat, AT_EACCESS, AT_FDCWD, DIR,
    FILE,
};
use mirrord_layer_macro::{hook_fn, hook_guard_fn};
use mirrord_protocol::file::{
    DirEntryInternal, MetadataInternal, OpenOptionsInternal, ReadFileResponse, WriteFileResponse,
};
use num_traits::Bounded;
use tracing::{log::error, trace};

use super::{ops::*, OpenOptionsInternalExt, OPEN_FILES};
use crate::{
    close_layer_fd,
    detour::{Detour, DetourGuard},
    error::HookError,
    file::ops::{access, lseek, open, read, write},
    hooks::HookManager,
    replace,
};

/// Implementation of open_detour, used in open_detour and openat_detour
/// We ignore mode in case we don't bypass the call.
#[tracing::instrument(level = "trace")]
unsafe fn open_logic(raw_path: *const c_char, open_flags: c_int, mode: c_int) -> RawFd {
    let rawish_path = (!raw_path.is_null()).then(|| CStr::from_ptr(raw_path));
    let open_options = OpenOptionsInternalExt::from_flags(open_flags);

    trace!(
        "rawish_path {:#?} | open_options {:#?}",
        rawish_path,
        open_options
    );

    open(rawish_path, open_options).unwrap_or_bypass_with(|_| FN_OPEN(raw_path, open_flags, mode))
}

/// Hook for `libc::open`.
///
/// **Bypassed** by `raw_path`s that match `IGNORE_FILES` regex.
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
        open_logic(raw_path, open_flags, mode)
    }
}

/// Hook for `libc::fopen`.
///
/// **Bypassed** by `raw_path`s that match `IGNORE_FILES` regex.
#[hook_guard_fn]
pub(super) unsafe extern "C" fn fopen_detour(
    raw_path: *const c_char,
    raw_mode: *const c_char,
) -> *mut FILE {
    let rawish_path = (!raw_path.is_null()).then(|| CStr::from_ptr(raw_path));
    let rawish_mode = (!raw_mode.is_null()).then(|| CStr::from_ptr(raw_mode));

    fopen(rawish_path, rawish_mode).unwrap_or_bypass_with(|_| FN_FOPEN(raw_path, raw_mode))
}

/// Hook for `libc::fdopen`.
///
/// Converts a `RawFd` into `*mut FILE` only for files that are already being managed by
/// mirrord-layer.
#[hook_guard_fn]
pub(super) unsafe extern "C" fn fdopen_detour(fd: RawFd, raw_mode: *const c_char) -> *mut FILE {
    let rawish_mode = (!raw_mode.is_null()).then(|| CStr::from_ptr(raw_mode));

    fdopen(fd, rawish_mode).unwrap_or_bypass_with(|_| FN_FDOPEN(fd, raw_mode))
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn fdopendir_detour(fd: RawFd) -> usize {
    fdopendir(fd).unwrap_or_bypass_with(|_| FN_FDOPENDIR(fd))
}

/// Assign DirEntryInternal to dirent
unsafe fn assign_direntry(
    in_entry: DirEntryInternal,
    out_entry: *mut dirent,
) -> Result<(), HookError> {
    let dir_name = CString::new(in_entry.name)?;
    let dir_name_bytes = dir_name.as_bytes_with_nul();

    (*out_entry).d_ino = in_entry.inode;
    (*out_entry).d_reclen = std::mem::size_of::<dirent>() as u16;
    (*out_entry).d_type = in_entry.file_type;
    (*out_entry).d_name[..dir_name_bytes.len()]
        .copy_from_slice(bytemuck::cast_slice(dir_name_bytes));

    #[cfg(target_os = "macos")]
    {
        (*out_entry).d_seekoff = in_entry.position;
        // name length should be without null
        (*out_entry).d_namlen = dir_name.to_bytes().len() as u16;
    }

    #[cfg(target_os = "linux")]
    {
        (*out_entry).d_off = in_entry.position as i64;
    }
    Ok(())
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn readdir_r_detour(
    dirp: *mut DIR,
    entry: *mut dirent,
    result: *mut *mut dirent,
) -> c_int {
    readdir_r(dirp as usize)
        .map(|resp| {
            if let Some(direntry) = resp {
                match assign_direntry(direntry, entry) {
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

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn closedir_detour(dirp: *mut DIR) -> c_int {
    closedir(dirp as usize).unwrap_or_bypass_with(|_| FN_CLOSEDIR(dirp))
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
    let rawish_path = (!raw_path.is_null()).then(|| CStr::from_ptr(raw_path));
    let open_options: OpenOptionsInternal = OpenOptionsInternalExt::from_flags(open_flags);

    openat(fd, rawish_path, open_options)
        .unwrap_or_bypass_with(|_| FN_OPENAT(fd, raw_path, open_flags))
}

/// Hook for getdents64, for Go on Linux.
// #[cfg(target_os = "linux")] // TODO: uncomment.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn getdents64_detour(
    fd: RawFd,
    dirent_buf: *mut c_void,
    buf_size: c_size_t,
) -> c_ssize_t {
    getdents64(fd, buf_size as u64)
        .map(|res| {
            if let Some(remote_errno) = res.errno {
                set_errno(Errno(remote_errno));
                -1 as c_ssize_t
            } else {
                let end = dirent_buf.byte_add(buf_size);
                let mut next = dirent_buf as *mut dirent; // TODO: use dirent64?
                for dent in res.entries {
                    // TODO: check for buffer overflow.
                    match assign_direntry(dent, next) {
                        Err(e) => {
                            error!("Error while trying to write remote dir entry to local buffer: {e:?}");
                            set_errno(Errno(c_int::from(e)));
                            return -1
                        },
                        Ok(()) => next = next.byte_add((*next).d_reclen as usize),
                    }
                }
                res.return_value as c_ssize_t
            }
        })
        .unwrap_or_bypass_with(|_| 0 /* TODO: call getdents64 syscall */)
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

/// Hook for `libc::fread`.
///
/// Reads `element_size * number_of_elements` bytes into `out_buffer`, only for `*mut FILE`s that
/// are being managed by mirrord-layer.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn fread_detour(
    out_buffer: *mut c_void,
    element_size: size_t,
    number_of_elements: size_t,
    file_stream: *mut FILE,
) -> size_t {
    // Extract the fd from stream and check if it's managed by us, or should be bypassed.
    let fd = fileno_logic(file_stream);
    let read_amount = (element_size * number_of_elements) as u64;

    read(fd, read_amount)
        .map(|read_file| {
            let ReadFileResponse { bytes, read_amount } = read_file;

            // There is no distinction between reading 0 bytes or if we hit EOF, but we only copy to
            // buffer if we have something to copy.
            if read_amount > 0 {
                let read_ptr = bytes.as_ptr();
                let out_buffer = out_buffer.cast();
                ptr::copy(read_ptr, out_buffer, read_amount as usize);
            }

            // TODO: The function fread() does not distinguish between end-of-file and error,
            // and callers must use feof(3) and ferror(3) to determine which occurred.
            read_amount as usize
        })
        .unwrap_or_bypass_with(|_| {
            FN_FREAD(out_buffer, element_size, number_of_elements, file_stream)
        })
}

/// Reads at most `capacity - 1` characters. Reading stops on `'\n'`, `EOF` or on error. On success,
/// it appends `'\0'` to the end of the string (thus the limit on `capacity`).
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn fgets_detour(
    out_buffer: *mut c_char,
    capacity: c_int,
    file_stream: *mut FILE,
) -> *mut c_char {
    // Extract the fd from stream and check if it's managed by us, or should be bypassed.
    let fd = fileno_logic(file_stream);

    // `fgets` reads 1 LESS character than specified by `capacity`, so instead of having
    // branching code to check if this is an `fgets` call elsewhere, we just subtract 1 from
    // `capacity` here.
    let buffer_size = capacity - 1;

    fgets(fd, buffer_size as usize)
        .map(|read_file| {
            let ReadFileResponse { bytes, read_amount } = read_file;

            // There is no distinction between reading 0 bytes or if we hit EOF, but we only
            // copy to buffer if we have something to copy.
            //
            // Callers can check for EOF by using `ferror`.
            if read_amount > 0 {
                let bytes_slice = &bytes[0..buffer_size.min(read_amount as i32) as usize];
                let eof = vec![0; 1];

                // Append '\0' to comply with `fgets`.
                let read = [bytes_slice, &eof].concat();

                ptr::copy(read.as_ptr().cast(), out_buffer, read.len());

                out_buffer
            } else {
                ptr::null_mut()
            }
        })
        .unwrap_or_bypass_with(|_| FN_FGETS(out_buffer, capacity, file_stream))
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
                let bytes_slice = &bytes[0..fixed_read as usize];

                ptr::copy(bytes_slice.as_ptr().cast(), out_buffer, bytes_slice.len());
            }
            fixed_read as ssize_t
        })
        .unwrap_or_bypass_with(|_| FN_PREAD(fd, out_buffer, amount_to_read, offset))
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn pwrite_detour(
    fd: RawFd,
    in_buffer: *const c_void,
    amount_to_write: size_t,
    offset: off_t,
) -> ssize_t {
    // Convert the given buffer into a u8 slice, upto the amount to write.
    let casted_in_buffer: &[u8] = slice::from_raw_parts(in_buffer.cast(), amount_to_write);

    pwrite(fd, casted_in_buffer, offset as u64)
        .map(|write_response| {
            let WriteFileResponse { written_amount } = write_response;
            written_amount as ssize_t
        })
        .unwrap_or_bypass_with(|_| FN_PWRITE(fd, in_buffer, amount_to_write, offset))
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

/// Implementation of access_detour, used in access_detour and faccessat_detour
unsafe fn access_logic(raw_path: *const c_char, mode: c_int) -> c_int {
    let rawish_path = (!raw_path.is_null()).then(|| CStr::from_ptr(raw_path));

    access(rawish_path, mode as u8).unwrap_or_bypass_with(|_| FN_ACCESS(raw_path, mode))
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

/// Used to distinguish between an error or `EOF` (especially relevant for `fgets`).
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn ferror_detour(file_stream: *mut FILE) -> c_int {
    // Extract the fd from stream and check if it's managed by us, or should be bypassed.
    let fd = fileno_logic(file_stream);

    // We're only interested in files that are handled by `mirrord-agent`.
    let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).map(|file| file.fd);
    if remote_fd.is_some() {
        std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or_default()
    } else {
        FN_FERROR(file_stream)
    }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn fclose_detour(file_stream: *mut FILE) -> c_int {
    let res = FN_FCLOSE(file_stream);
    // Extract the fd from stream and check if it's managed by us, or should be bypassed.
    let fd = fileno_logic(file_stream);

    close_layer_fd(fd);
    res
}

/// Hook for `libc::fileno`.
///
/// Converts a `*mut FILE` stream into an fd.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn fileno_detour(file_stream: *mut FILE) -> c_int {
    fileno_logic(file_stream)
}

/// Implementation of fileno_detour, used in fileno_detour and fread_detour
unsafe fn fileno_logic(file_stream: *mut FILE) -> c_int {
    let local_fd = *(file_stream as *const _);

    if OPEN_FILES.lock().unwrap().contains_key(&local_fd) {
        local_fd
    } else {
        FN_FILENO(file_stream)
    }
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
unsafe extern "C" fn fill_stat(out_stat: *mut stat, metadata: &MetadataInternal) {
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

/// Hook for `libc::lstat`.
#[hook_guard_fn]
unsafe extern "C" fn lstat_detour(raw_path: *const c_char, out_stat: *mut stat) -> c_int {
    let path = (!raw_path.is_null()).then(|| CStr::from_ptr(raw_path));
    xstat(Some(path), None, false)
        .map(|res| {
            let res = res.metadata;
            fill_stat(out_stat, &res);
            0
        })
        .unwrap_or_bypass_with(|_| FN_LSTAT(raw_path, out_stat))
}

/// Hook for `libc::fstat`.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn fstat_detour(fd: RawFd, out_stat: *mut stat) -> c_int {
    xstat(None, Some(fd), true)
        .map(|res| {
            let res = res.metadata;
            fill_stat(out_stat, &res);
            0
        })
        .unwrap_or_bypass_with(|_| FN_FSTAT(fd, out_stat))
}

/// Hook for `libc::stat`.
#[hook_guard_fn]
unsafe extern "C" fn stat_detour(raw_path: *const c_char, out_stat: *mut stat) -> c_int {
    let path = (!raw_path.is_null()).then(|| CStr::from_ptr(raw_path));
    xstat(Some(path), None, true)
        .map(|res| {
            let res = res.metadata;
            fill_stat(out_stat, &res);
            0
        })
        .unwrap_or_bypass_with(|_| FN_STAT(raw_path, out_stat))
}

/// Hook for libc's stat syscall wrapper.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn __xstat_detour(
    ver: c_int,
    raw_path: *const c_char,
    out_stat: *mut stat,
) -> c_int {
    if ver != 1 {
        return FN___XSTAT(ver, raw_path, out_stat);
    }
    let path = (!raw_path.is_null()).then(|| CStr::from_ptr(raw_path));
    xstat(Some(path), None, true)
        .map(|res| {
            let res = res.metadata;
            fill_stat(out_stat, &res);
            0
        })
        .unwrap_or_bypass_with(|_| FN___XSTAT(ver, raw_path, out_stat))
}

/// Separated out logic for `fstatat` so that it can be used by go to match on the xstat result.
pub(crate) unsafe fn fstatat_logic(
    fd: RawFd,
    raw_path: *const c_char,
    out_stat: *mut stat,
    flag: c_int,
) -> Detour<i32> {
    let follow_symlink = (flag & libc::AT_SYMLINK_NOFOLLOW) == 0;
    let path = (!raw_path.is_null()).then(|| CStr::from_ptr(raw_path));
    xstat(Some(path), Some(fd), follow_symlink).map(|res| {
        let res = res.metadata;
        fill_stat(out_stat, &res);
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
    fstatat_logic(fd, raw_path, out_stat, flag)
        .unwrap_or_bypass_with(|_| FN_FSTATAT(fd, raw_path, out_stat, flag))
}

// this requires newer libc which we don't link with to support old libc..
// leaving this in code so we can enable it when libc is updated.
// #[cfg(target_os = "linux")]
// /// Hook for `libc::statx`. This is only available on Linux.
// #[hook_guard_fn]
// unsafe extern "C" fn statx_detour(
//     fd: RawFd,
//     raw_path: *const c_char,
//     flag: c_int,
//     out_stat: *mut statx,
// ) -> c_int {
//     let follow_symlink = (flag & libc::AT_SYMLINK_NOFOLLOW) == 0;
//     let path = if flags & libc::AT_EMPTYPATH != 0 {
//         None
//     } else {
//         Some((!raw_path.is_null()).then(|| CStr::from_ptr(raw_path)))
//     };
//     let (Ok(result) | Err(result)) = xstat(path, Some(fd), follow_symlink)
//         .map(|res| {
//             let res = res.metadata;
//             out_stat.write_bytes(0, 1);
//             let out = &mut *out_stat;
//             // Set mask for available fields
//             out.stx_mask = libc::STATX_BASIC_STATS;
//             out.stx_mode = best_effort_cast(metadata.mode);
//             out.stx_size = best_effort_cast(metadata.size);
//             out.stx_atime.tv_nsec = metadata.access_time;
//             out.stx_mtime.tv_nsec = metadata.modification_time;
//             out.stx_ctime.tv_nsec = metadata.creation_time;
//             out.stx_atime.tv_sec = nano_to_secs(metadata.access_time);
//             out.stx_mtime.tv_sec = nano_to_secs(metadata.modification_time);
//             out.stx_ctime.tv_sec = nano_to_secs(metadata.creation_time);
//             out.stx_nlink = best_effort_cast(metadata.hard_links);
//             out.stx_uid = metadata.user_id;
//             out.stx_gid = metadata.group_id;
//             out.stx_ino = best_effort_cast(metadata.inode);
//             out.stx_blksize = best_effort_cast(metadata.block_size);
//             out.stx_blocks = best_effort_cast(metadata.blocks);

//             out.stx_dev_major = libc::major(metadata.device_id);
//             out.stx_dev_minor = libc::minor(metadata.device_id);
//             out.stx_rdev_major = libc::major(metadata.rdevice_id);
//             out.stx_rdev_minor = libc::minor(metadata.rdevice_id);
//             0
//         })
//         .unwrap_or_bypass_with(|_| FN_STATX(fd, raw_path, flag, out_stat))
//         .map_err(From::from);

//     result
// }

/// Convenience function to setup file hooks (`x_detour`) with `frida_gum`.
pub(crate) unsafe fn enable_file_hooks(hook_manager: &mut HookManager) {
    replace!(hook_manager, "open", open_detour, FnOpen, FN_OPEN);
    replace!(hook_manager, "openat", openat_detour, FnOpenat, FN_OPENAT);
    replace!(hook_manager, "fopen", fopen_detour, FnFopen, FN_FOPEN);
    replace!(hook_manager, "fdopen", fdopen_detour, FnFdopen, FN_FDOPEN);

    replace!(hook_manager, "read", read_detour, FnRead, FN_READ);

    replace!(
        hook_manager,
        "closedir",
        closedir_detour,
        FnClosedir,
        FN_CLOSEDIR
    );
    replace!(hook_manager, "fread", fread_detour, FnFread, FN_FREAD);
    replace!(hook_manager, "fgets", fgets_detour, FnFgets, FN_FGETS);
    replace!(hook_manager, "pread", pread_detour, FnPread, FN_PREAD);
    replace!(hook_manager, "ferror", ferror_detour, FnFerror, FN_FERROR);
    replace!(hook_manager, "fclose", fclose_detour, FnFclose, FN_FCLOSE);
    replace!(hook_manager, "fileno", fileno_detour, FnFileno, FN_FILENO);
    replace!(hook_manager, "lseek", lseek_detour, FnLseek, FN_LSEEK);
    replace!(hook_manager, "write", write_detour, FnWrite, FN_WRITE);
    replace!(hook_manager, "pwrite", pwrite_detour, FnPwrite, FN_PWRITE);
    replace!(hook_manager, "access", access_detour, FnAccess, FN_ACCESS);
    replace!(
        hook_manager,
        "faccessat",
        faccessat_detour,
        FnFaccessat,
        FN_FACCESSAT
    );
    // this requires newer libc which we don't link with to support old libc..
    // leaving this in code so we can enable it when libc is updated.
    // #[cfg(target_os = "linux")]
    // {
    //     replace!(hook_manager, "statx", statx_detour, FnStatx, FN_STATX);
    // }
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    {
        replace!(
            hook_manager,
            "__xstat",
            __xstat_detour,
            Fn__xstat,
            FN___XSTAT
        );
        replace!(hook_manager, "lstat", lstat_detour, FnLstat, FN_LSTAT);
        replace!(hook_manager, "fstat", fstat_detour, FnFstat, FN_FSTAT);
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
    }
}
