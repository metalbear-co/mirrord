/// FFI functions that override the `libc` calls (see `file` module documentation on how to
/// enable/disable these).
///
/// NOTICE: If a file operation fails, it might be because it depends on some `libc` function
/// that is not being hooked (`strace` the program to check).
use std::{ffi::CStr, os::unix::io::RawFd, ptr, slice};

use frida_gum::interceptor::Interceptor;
use libc::{self, c_char, c_int, c_void, off_t, size_t, ssize_t, AT_EACCESS, AT_FDCWD, FILE};
use mirrord_macro::hook_guard_fn;
use mirrord_protocol::{OpenOptionsInternal, ReadFileResponse, WriteFileResponse};
use tracing::debug;

use super::{ops::*, OpenOptionsInternalExt, OPEN_FILES};
use crate::{
    close_detour,
    file::ops::{access, lseek, open, read, write},
    replace,
};

/// Implementation of open_detour, used in open_detour and openat_detour
#[tracing::instrument(level = "trace")]
unsafe fn open_logic(raw_path: *const c_char, open_flags: c_int) -> RawFd {
    let rawish_path = (!raw_path.is_null()).then(|| CStr::from_ptr(raw_path));
    let open_options: OpenOptionsInternal = OpenOptionsInternalExt::from_flags(open_flags);

    debug!(
        "open_logic -> rawish_path {:#?} | open_options {:#?}",
        rawish_path, open_options
    );

    let (Ok(result) | Err(result)) = open(rawish_path, open_options)
        .bypass_with(|_| FN_OPEN(raw_path, open_flags))
        .map_err(From::from);

    result
}

/// Hook for `libc::open`.
///
/// **Bypassed** by `raw_path`s that match `IGNORE_FILES` regex.
#[hook_guard_fn]
pub(super) unsafe extern "C" fn open_detour(raw_path: *const c_char, open_flags: c_int) -> RawFd {
    open_logic(raw_path, open_flags)
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

    let (Ok(result) | Err(result)) = fopen(rawish_path, rawish_mode)
        .bypass_with(|_| FN_FOPEN(raw_path, raw_mode))
        .map_err(From::from);

    result
}

/// Hook for `libc::fdopen`.
///
/// Converts a `RawFd` into `*mut FILE` only for files that are already being managed by
/// mirrord-layer.
#[hook_guard_fn]
pub(super) unsafe extern "C" fn fdopen_detour(fd: RawFd, raw_mode: *const c_char) -> *mut FILE {
    let rawish_mode = (!raw_mode.is_null()).then(|| CStr::from_ptr(raw_mode));

    let (Ok(result) | Err(result)) = fdopen(fd, rawish_mode)
        .bypass_with(|_| FN_FDOPEN(fd, raw_mode))
        .map_err(From::from);
    result
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

    let (Ok(result) | Err(result)) = openat(fd, rawish_path, open_options)
        .bypass_with(|_| FN_OPENAT(fd, raw_path, open_flags))
        .map_err(From::from);

    result
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
    let (Ok(result) | Err(result)) = read(fd, count)
        .map(|read_file| {
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
            ssize_t::try_from(read_amount).unwrap()
        })
        .bypass_with(|_| FN_READ(fd, out_buffer, count))
        .map_err(From::from);

    result
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
    let read_amount = element_size * number_of_elements;

    let (Ok(result) | Err(result)) = read(fd, read_amount)
        .map(|read_file| {
            let ReadFileResponse { bytes, read_amount } = read_file;

            // There is no distinction between reading 0 bytes or if we hit EOF, but we only copy to
            // buffer if we have something to copy.
            if read_amount > 0 {
                let read_ptr = bytes.as_ptr();
                let out_buffer = out_buffer.cast();
                ptr::copy(read_ptr, out_buffer, read_amount);
            }

            // TODO: The function fread() does not distinguish between end-of-file and error,
            // and callers must use feof(3) and ferror(3) to determine which occurred.
            read_amount
        })
        .bypass_with(|_| FN_FREAD(out_buffer, element_size, number_of_elements, file_stream))
        .map_err(From::from);

    result
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
    let buffer_size = (capacity - 1) as usize;

    let (Ok(result) | Err(result)) = fgets(fd, buffer_size)
        .map(|read_file| {
            let ReadFileResponse { bytes, read_amount } = read_file;

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
        })
        .bypass_with(|_| FN_FGETS(out_buffer, capacity, file_stream))
        .map_err(From::from);

    result
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn pread_detour(
    fd: RawFd,
    out_buffer: *mut c_void,
    amount_to_read: size_t,
    offset: off_t,
) -> ssize_t {
    let (Ok(result) | Err(result)) = pread(fd, amount_to_read as usize, offset as u64)
        .map(|read_file| {
            let ReadFileResponse { bytes, read_amount } = read_file;
            let fixed_read = amount_to_read.min(read_amount);

            // There is no distinction between reading 0 bytes or if we hit EOF, but we only
            // copy to buffer if we have something to copy.
            //
            // Callers can check for EOF by using `ferror`.
            if read_amount > 0 {
                let bytes_slice = &bytes[0..fixed_read];

                ptr::copy(bytes_slice.as_ptr().cast(), out_buffer, bytes_slice.len());
            }
            fixed_read as ssize_t
        })
        .bypass_with(|_| FN_PREAD(fd, out_buffer, amount_to_read, offset))
        .map_err(From::from);

    result
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

    let (Ok(result) | Err(result)) = pwrite(fd, casted_in_buffer, offset as u64)
        .map(|write_response| {
            let WriteFileResponse { written_amount } = write_response;
            written_amount as ssize_t
        })
        .bypass_with(|_| FN_PWRITE(fd, in_buffer, amount_to_write, offset))
        .map_err(From::from);

    result
}

/// Hook for `libc::lseek`.
///
/// **Bypassed** by `fd`s that are not managed by us (not found in `OPEN_FILES`).
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn lseek_detour(fd: RawFd, offset: off_t, whence: c_int) -> off_t {
    let (Ok(result) | Err(result)) = lseek(fd, offset, whence)
        .map(|offset| i64::try_from(offset).unwrap())
        .bypass_with(|_| FN_LSEEK(fd, offset, whence))
        .map_err(From::from);

    result
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

    let (Ok(result) | Err(result)) = write(fd, write_bytes)
        .bypass_with(|_| FN_WRITE(fd, buffer, count))
        .map_err(From::from);

    result
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

/// Implementation of access_detour, used in access_detour and faccessat_detour
unsafe fn access_logic(raw_path: *const c_char, mode: c_int) -> c_int {
    let rawish_path = (!raw_path.is_null()).then(|| CStr::from_ptr(raw_path));

    let (Ok(result) | Err(result)) = access(rawish_path, mode as u8)
        .bypass_with(|_| FN_ACCESS(raw_path, mode))
        .map_err(From::from);

    result
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

/// Convenience function to setup file hooks (`x_detour`) with `frida_gum`.
pub(crate) unsafe fn enable_file_hooks(interceptor: &mut Interceptor) {
    let _ = replace!(interceptor, "open", open_detour, FnOpen, FN_OPEN);
    let _ = replace!(interceptor, "openat", openat_detour, FnOpenat, FN_OPENAT);
    let _ = replace!(interceptor, "fopen", fopen_detour, FnFopen, FN_FOPEN);
    let _ = replace!(interceptor, "fdopen", fdopen_detour, FnFdopen, FN_FDOPEN);
    let _ = replace!(interceptor, "read", read_detour, FnRead, FN_READ);
    let _ = replace!(interceptor, "fread", fread_detour, FnFread, FN_FREAD);
    let _ = replace!(interceptor, "fgets", fgets_detour, FnFgets, FN_FGETS);
    let _ = replace!(interceptor, "pread", pread_detour, FnPread, FN_PREAD);
    let _ = replace!(interceptor, "ferror", ferror_detour, FnFerror, FN_FERROR);
    let _ = replace!(interceptor, "fclose", fclose_detour, FnFclose, FN_FCLOSE);
    let _ = replace!(interceptor, "fileno", fileno_detour, FnFileno, FN_FILENO);
    let _ = replace!(interceptor, "lseek", lseek_detour, FnLseek, FN_LSEEK);
    let _ = replace!(interceptor, "write", write_detour, FnWrite, FN_WRITE);
    let _ = replace!(interceptor, "pwrite", pwrite_detour, FnPwrite, FN_PWRITE);
    let _ = replace!(interceptor, "access", access_detour, FnAccess, FN_ACCESS);
    let _ = replace!(
        interceptor,
        "faccessat",
        faccessat_detour,
        FnFaccessat,
        FN_FACCESSAT
    );
}
