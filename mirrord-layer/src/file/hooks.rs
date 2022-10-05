use std::{ffi::CStr, io::SeekFrom, os::unix::io::RawFd, path::PathBuf, ptr, slice};

use frida_gum::interceptor::Interceptor;
use libc::{self, c_char, c_int, c_void, off_t, size_t, ssize_t, AT_EACCESS, AT_FDCWD, FILE};
use mirrord_macro::hook_guard_fn;
use mirrord_protocol::{OpenOptionsInternal, ReadFileResponse, ReadLineFileResponse};
use tracing::{debug, error};

use super::{ops::*, OpenOptionsInternalExt, IGNORE_FILES, OPEN_FILES};
use crate::{
    close_detour,
    error::HookError,
    file::ops::{access, lseek, open, read, write},
    replace, ENABLED_FILE_RO_OPS,
};

/// Hook for `libc::open`.
///
/// **Bypassed** by `raw_path`s that match `IGNORE_FILES` regex.
#[hook_guard_fn]
pub(super) unsafe extern "C" fn open_detour(raw_path: *const c_char, open_flags: c_int) -> RawFd {
    open_logic(raw_path, open_flags)
}

/// Implementation of open_detour, used in open_detour and openat_detour
unsafe fn open_logic(raw_path: *const c_char, open_flags: c_int) -> RawFd {
    let path = match CStr::from_ptr(raw_path)
        .to_str()
        .map_err(HookError::from)
        .map(PathBuf::from)
    {
        Ok(path) => path,
        Err(fail) => return fail.into(),
    };

    // Calls with non absolute paths are sent to libc::open.
    if IGNORE_FILES.is_match(path.to_str().unwrap_or_default()) || !path.is_absolute() {
        FN_OPEN(raw_path, open_flags)
    } else {
        debug!("open_logic -> path {path:#?}");

        let open_options: OpenOptionsInternal = OpenOptionsInternalExt::from_flags(open_flags);
        let read_only = ENABLED_FILE_RO_OPS
            .get()
            .expect("Should be set during initialization!");
        if *read_only && !open_options.is_read_only() {
            return FN_OPEN(raw_path, open_flags);
        }
        let open_result = open(path, open_options);

        let (Ok(result) | Err(result)) = open_result.map_err(From::from);
        result
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
    let path = match CStr::from_ptr(raw_path)
        .to_str()
        .map_err(HookError::from)
        .map(PathBuf::from)
    {
        Ok(path) => path,
        Err(fail) => return fail.into(),
    };

    let mode = match CStr::from_ptr(raw_mode)
        .to_str()
        .map(String::from)
        .map_err(HookError::from)
    {
        Ok(mode) => mode,
        Err(fail) => return fail.into(),
    };

    if IGNORE_FILES.is_match(path.to_str().unwrap()) || !path.is_absolute() {
        FN_FOPEN(raw_path, raw_mode)
    } else {
        debug!("fopen_detour -> path {path:#?} | mode {mode:#?}");

        let open_options: OpenOptionsInternal = OpenOptionsInternalExt::from_mode(mode);

        let read_only = ENABLED_FILE_RO_OPS
            .get()
            .expect("Should be set during initialization!");
        if *read_only && !open_options.is_read_only() {
            return FN_FOPEN(raw_path, raw_mode);
        }
        let fopen_result = fopen(path, open_options);

        let (Ok(result) | Err(result)) = fopen_result.map_err(From::from);
        result
    }
}

/// Hook for `libc::fdopen`.
///
/// Converts a `RawFd` into `*mut FILE` only for files that are already being managed by
/// mirrord-layer.
#[hook_guard_fn]
pub(super) unsafe extern "C" fn fdopen_detour(fd: RawFd, raw_mode: *const c_char) -> *mut FILE {
    let mode = match CStr::from_ptr(raw_mode)
        .to_str()
        .map(String::from)
        .map_err(HookError::from)
    {
        Ok(mode) => mode,
        Err(fail) => return fail.into(),
    };

    let open_files = OPEN_FILES.lock().unwrap();
    let open_file = open_files.get_key_value(&fd);

    if let Some((local_fd, remote_fd)) = open_file {
        let open_options = OpenOptionsInternalExt::from_mode(mode);

        let fdopen_result = fdopen(local_fd, *remote_fd, open_options);

        let (Ok(result) | Err(result)) = fdopen_result.map_err(From::from);
        result
    } else {
        FN_FDOPEN(fd, raw_mode)
    }
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
    let path = match CStr::from_ptr(raw_path)
        .to_str()
        .map_err(HookError::from)
        .map(PathBuf::from)
    {
        Ok(path) => path,
        Err(fail) => return fail.into(),
    };

    debug!("openat_detour -> path {:#?}", path);

    // `openat` behaves the same as `open` when the path is absolute.
    // when called with AT_FDCWD, the call is propagated to `open`.

    if path.is_absolute() || fd == AT_FDCWD {
        open_logic(raw_path, open_flags)
    } else {
        // Relative path requires special handling, we must identify the relative part (relative to
        // what).
        let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).cloned();

        // Are we managing the relative part?
        if let Some(remote_fd) = remote_fd {
            let openat_result = openat(path, open_flags, remote_fd);

            let (Ok(result) | Err(result)) = openat_result.map_err(From::from);
            result
        } else {
            // Nope, it's relative outside of our hands.

            FN_OPENAT(fd, raw_path, open_flags)
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
    // We're only interested in files that are paired with mirrord-agent.
    let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).cloned();

    if let Some(remote_fd) = remote_fd {
        let read_result = read(remote_fd, count).map(|read_file| {
            let ReadFileResponse { bytes, read_amount } = read_file;

            // There is no distinction between reading 0 bytes or if we hit EOF, but we only copy to
            // buffer if we have something to copy.
            if read_amount > 0 {
                let read_ptr = bytes.as_ptr();
                let out_buffer = out_buffer.cast();
                ptr::copy(read_ptr, out_buffer, read_amount);
            }

            // WARN: Must be careful when it comes to `EOF`, incorrect handling may appear as the
            // `read` call being repeated.
            read_amount.try_into().unwrap()
        });

        let (Ok(result) | Err(result)) = read_result.map_err(From::from);
        result
    } else {
        FN_READ(fd, out_buffer, count)
    }
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

    // We're only interested in files that are handled by `mirrord-agent`.
    let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).cloned();
    if let Some(remote_fd) = remote_fd {
        let read_result = read(remote_fd, element_size * number_of_elements).map(|read_file| {
            let ReadFileResponse { bytes, read_amount } = read_file;

            // There is no distinction between reading 0 bytes or if we hit EOF, but we only
            // copy to buffer if we have something to copy.
            if read_amount > 0 {
                let read_ptr = bytes.as_ptr();
                let out_buffer = out_buffer.cast();
                ptr::copy(read_ptr, out_buffer, read_amount);
            }

            // TODO: The function fread() does not distinguish between end-of-file and error,
            // and callers must use feof(3) and ferror(3) to determine which occurred.
            read_amount
        });

        let (Ok(result) | Err(result)) = read_result.map_err(From::from);
        result
    } else {
        FN_FREAD(out_buffer, element_size, number_of_elements, file_stream)
    }
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

    // We're only interested in files that are handled by `mirrord-agent`.
    let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).cloned();
    if let Some(remote_fd) = remote_fd {
        // `fgets` reads 1 LESS character than specified by `capacity`, so instead of having
        // branching code to check if this is an `fgets` call elsewhere, we just subtract 1 from
        // `capacity` here.
        let buffer_size = (capacity - 1) as usize;

        let read_result = fgets(remote_fd, buffer_size).map(|read_file| {
            let ReadLineFileResponse { bytes, read_amount } = read_file;

            // There is no distinction between reading 0 bytes or if we hit EOF, but we only
            // copy to buffer if we have something to copy.
            //
            // Callers can check for EOF by using `ferror`.
            if read_amount > 0 {
                let bytes_slice = &bytes[0..buffer_size.min(read_amount)];
                let eof = vec![0; 1];

                // Append '\0' to comply with `fgets`.
                let read = [bytes_slice, &eof].concat();

                ptr::copy(read.as_ptr().cast(), out_buffer, read.len());

                out_buffer
            } else {
                ptr::null_mut()
            }
        });

        let (Ok(result) | Err(result)) = read_result.map_err(From::from);
        result
    } else {
        FN_FGETS(out_buffer, capacity, file_stream)
    }
}

/// Used to distinguish between an error or `EOF` (especially relevant for `fgets`).
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn ferror_detour(file_stream: *mut FILE) -> c_int {
    // Extract the fd from stream and check if it's managed by us, or should be bypassed.
    let fd = fileno_logic(file_stream);

    // We're only interested in files that are handled by `mirrord-agent`.
    let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).cloned();
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
    // Extract the fd from stream and check if it's managed by us, or should be bypassed.
    let fd = fileno_logic(file_stream);

    close_detour(fd)
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

/// Hook for `libc::lseek`.
///
/// **Bypassed** by `fd`s that are not managed by us (not found in `OPEN_FILES`).
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn lseek_detour(fd: RawFd, offset: off_t, whence: c_int) -> off_t {
    let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).cloned();

    if let Some(remote_fd) = remote_fd {
        let seek_from = match whence {
            libc::SEEK_SET => SeekFrom::Start(offset as u64),
            libc::SEEK_CUR => SeekFrom::Current(offset),
            libc::SEEK_END => SeekFrom::End(offset),
            invalid => {
                error!(
                    "lseek_detour -> potential invalid value {:#?} for whence {:#?}",
                    invalid, whence
                );
                return -1;
            }
        };

        let lseek_result = lseek(remote_fd, seek_from).map(|offset| offset.try_into().unwrap());

        let (Ok(result) | Err(result)) = lseek_result.map_err(From::from);
        result
    } else {
        FN_LSEEK(fd, offset, whence)
    }
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
    // Avoid writing to `std(in|out|err)`.
    if fd > 2 {
        write_logic(fd, buffer, count)
    } else {
        FN_WRITE(fd, buffer, count)
    }
}

/// Implementation of write_detour, used in  write_detour
pub(crate) unsafe extern "C" fn write_logic(
    fd: RawFd,
    buffer: *const c_void,
    count: size_t,
) -> ssize_t {
    let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).cloned();

    if let Some(remote_fd) = remote_fd {
        if buffer.is_null() {
            return -1;
        }

        // WARN: Be veeery careful here, you cannot construct the `Vec` directly, as the
        // buffer allocation is handled on the C side.
        let outside_buffer = slice::from_raw_parts(buffer as *const u8, count);
        let write_bytes = outside_buffer.to_vec();

        let write_result = write(remote_fd, write_bytes);

        let (Ok(result) | Err(result)) = write_result.map_err(From::from);
        result
    } else {
        FN_WRITE(fd, buffer, count)
    }
}

/// Hook for `libc::access`.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn access_detour(raw_path: *const c_char, mode: c_int) -> c_int {
    access_logic(raw_path, mode)
}

/// Implementation of access_detour, used in access_detour and faccessat_detour
unsafe fn access_logic(raw_path: *const c_char, mode: c_int) -> c_int {
    let path = match CStr::from_ptr(raw_path)
        .to_str()
        .map_err(HookError::from)
        .map(PathBuf::from)
    {
        Ok(path) => path,
        Err(fail) => return fail.into(),
    };

    // Calls with non absolute paths are sent to libc::open.
    if IGNORE_FILES.is_match(path.to_str().unwrap_or_default()) || !path.is_absolute() {
        FN_ACCESS(raw_path, mode)
    } else {
        let access_result = access(path, mode as u8);

        let (Ok(result) | Err(result)) = access_result.map_err(From::from);
        result
    }
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

/// Convenience function to setup file hooks (`x_detour`) with `frida_gum`.
pub(crate) unsafe fn enable_file_hooks(interceptor: &mut Interceptor) {
    let _ = replace!(interceptor, "open", open_detour, FnOpen, FN_OPEN);
    let _ = replace!(interceptor, "openat", openat_detour, FnOpenat, FN_OPENAT);
    let _ = replace!(interceptor, "fopen", fopen_detour, FnFopen, FN_FOPEN);
    let _ = replace!(interceptor, "fdopen", fdopen_detour, FnFdopen, FN_FDOPEN);
    let _ = replace!(interceptor, "read", read_detour, FnRead, FN_READ);
    let _ = replace!(interceptor, "fread", fread_detour, FnFread, FN_FREAD);
    let _ = replace!(interceptor, "fgets", fgets_detour, FnFgets, FN_FGETS);
    let _ = replace!(interceptor, "ferror", ferror_detour, FnFerror, FN_FERROR);
    let _ = replace!(interceptor, "fclose", fclose_detour, FnFclose, FN_FCLOSE);
    let _ = replace!(interceptor, "fileno", fileno_detour, FnFileno, FN_FILENO);
    let _ = replace!(interceptor, "lseek", lseek_detour, FnLseek, FN_LSEEK);
    let _ = replace!(interceptor, "write", write_detour, FnWrite, FN_WRITE);
    let _ = replace!(interceptor, "access", access_detour, FnAccess, FN_ACCESS);
    let _ = replace!(
        interceptor,
        "faccessat",
        faccessat_detour,
        FnFaccessat,
        FN_FACCESSAT
    );
}
