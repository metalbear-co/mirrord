use std::{ffi::CStr, io::SeekFrom, os::unix::io::RawFd, path::PathBuf, ptr, slice};

use frida_gum::interceptor::Interceptor;
use libc::{self, c_char, c_int, c_void, off_t, size_t, ssize_t, AT_FDCWD, FILE};
use mirrord_protocol::ReadFileResponse;
use tracing::error;

use super::{
    ops::{fdopen, fopen, openat},
    OpenOptionsInternalExt, IGNORE_FILES, OPEN_FILES,
};
use crate::{
    file::ops::{lseek, open, read, write},
    macros::hook,
};

/// Hook for `libc::open`.
///
/// **Bypassed** by `raw_path`s that match `IGNORE_FILES` regex.
pub(super) unsafe extern "C" fn open_detour(raw_path: *const c_char, open_flags: c_int) -> RawFd {
    let path: PathBuf = match CStr::from_ptr(raw_path).to_str().map_err(|fail| {
        error!(
            "Failed converting raw_path {:#?} from `c_char` with {:#?}",
            raw_path, fail
        );
        -1
    }) {
        Ok(path_str) => path_str.into(),
        Err(fail) => return fail,
    };

    // Calls with non absolute paths are sent to libc::open.
    if IGNORE_FILES.is_match(path.to_str().unwrap_or_default()) || !path.is_absolute() {
        libc::open(raw_path, open_flags)
    } else {
        let open_options = OpenOptionsInternalExt::from_flags(open_flags);
        let open_result = open(path.clone(), open_options);

        match open_result {
            Ok(fd) => fd,
            Err(fail) => {
                error!("Failed opening file {:#?} with {:#?}", path.clone(), fail);
                fail.into()
            }
        }
    }
}

/// Hook for `libc::fopen`.
///
/// **Bypassed** by `raw_path`s that match `IGNORE_FILES` regex.
pub(super) unsafe extern "C" fn fopen_detour(
    raw_path: *const c_char,
    raw_mode: *const c_char,
) -> *mut FILE {
    let path: PathBuf = match CStr::from_ptr(raw_path).to_str().map_err(|fail| {
        error!(
            "Failed converting raw_path {:#?} from `c_char` with {:#?}",
            raw_path, fail
        );
        std::ptr::null_mut()
    }) {
        Ok(path_str) => path_str.into(),
        Err(fail) => return fail,
    };

    let mode: String = match CStr::from_ptr(raw_mode).to_str().map_err(|fail| {
        error!(
            "Failed converting raw_mode {:#?} from `c_char` with {:#?}",
            raw_mode, fail
        );
        std::ptr::null_mut()
    }) {
        Ok(mode_str) => mode_str.into(),
        Err(fail) => return fail,
    };

    if IGNORE_FILES.is_match(path.to_str().unwrap()) || !path.is_absolute() {
        libc::fopen(raw_path, raw_mode)
    } else {
        let open_options = OpenOptionsInternalExt::from_mode(mode);
        let fopen_result = fopen(path.clone(), open_options);

        match fopen_result {
            Ok(file) => file,
            Err(fail) => {
                error!("Failed opening file {:#?} with {:#?}", path.clone(), fail);
                ptr::null_mut()
            }
        }
    }
}

/// Hook for `libc::fdopen`.
///
/// Converts a `RawFd` into `*mut FILE` only for files that are already being managed by
/// mirrord-layer.
pub(super) unsafe extern "C" fn fdopen_detour(fd: RawFd, raw_mode: *const c_char) -> *mut FILE {
    let mode: String = match CStr::from_ptr(raw_mode).to_str().map_err(|fail| {
        error!(
            "Failed converting raw_mode {:#?} from `c_char` with {:#?}",
            raw_mode, fail
        );
        std::ptr::null_mut()
    }) {
        Ok(mode_str) => mode_str.into(),
        Err(fail) => return fail,
    };

    let open_files = OPEN_FILES.lock().unwrap();
    let open_file = open_files.get_key_value(&fd);

    if let Some((local_fd, remote_fd)) = open_file {
        let open_options = OpenOptionsInternalExt::from_mode(mode);
        let fdopen_result = fdopen(local_fd, *remote_fd, open_options);

        match fdopen_result {
            Ok(file) => file,
            Err(fail) => {
                error!("Failed opening file with {:#?}", fail);
                ptr::null_mut()
            }
        }
    } else {
        libc::fdopen(fd, raw_mode)
    }
}

/// Equivalent to `open_detour`, **except** when `raw_path` specifies a relative path.
///
/// If `fd == AT_FDCWD`, the current working directory is used, and the behavior is the same as
/// `open_detour`.
/// `fd` for a file descriptor with the `O_DIRECTORY` flag.
pub(super) unsafe extern "C" fn openat_detour(
    fd: RawFd,
    raw_path: *const c_char,
    open_flags: c_int,
) -> RawFd {
    let path: PathBuf = match CStr::from_ptr(raw_path).to_str().map_err(|fail| {
        error!(
            "Failed converting raw_path {:#?} from `c_char` with {:#?}",
            raw_path, fail
        );
        -1
    }) {
        Ok(path_str) => path_str.into(),
        Err(fail) => return fail,
    };

    // `openat` behaves the same as `open` when the path is absolute.
    // when called with AT_FDCWD, the call is propagated to `open`.

    if path.is_absolute() || fd == AT_FDCWD {
        open_detour(raw_path, open_flags)
    } else {
        // Relative path requires special handling, we must identify the relative part (relative to
        // what).
        let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).cloned();

        // Are we managing the relative part?
        if let Some(remote_fd) = remote_fd {
            let openat_result = openat(path.clone(), open_flags, remote_fd);

            match openat_result {
                Ok(fd) => fd,
                Err(fail) => {
                    error!("Failed opening file {:#?} with {:#?}", path.clone(), fail);
                    fail.into()
                }
            }
        } else {
            // Nope, it's relative outside of our hands.

            libc::openat(fd, raw_path, open_flags)
        }
    }
}

/// Hook for `libc::read`.
///
/// Reads `count` bytes into `out_buffer`, only for `fd`s that are being managed by mirrord-layer.
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

        match read_result {
            Ok(read_amount) => read_amount,
            Err(fail) => {
                error!("Failed reading file with {:#?}", fail);
                -1
            }
        }
    } else {
        libc::read(fd, out_buffer, count)
    }
}

/// Hook for `libc::fread`.
///
/// Reads `element_size * number_of_elements` bytes into `out_buffer`, only for `*mut FILE`s that
/// are being managed by mirrord-layer.
pub(crate) unsafe extern "C" fn fread_detour(
    out_buffer: *mut c_void,
    element_size: size_t,
    number_of_elements: size_t,
    file_stream: *mut FILE,
) -> size_t {
    // Extract the fd from stream and check if it's managed by us, or should be bypassed.
    let fd = fileno_detour(file_stream);

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

        match read_result {
            Ok(read_amount) => read_amount,
            Err(fail) => {
                error!("Failed reading file with {:#?}", fail);
                fail.into()
            }
        }
    } else {
        libc::fread(out_buffer, element_size, number_of_elements, file_stream)
    }
}

/// Hook for `libc::fileno`.
///
/// Converts a `*mut FILE` stream into an fd.
pub(crate) unsafe extern "C" fn fileno_detour(file_stream: *mut FILE) -> c_int {
    let local_fd = *(file_stream as *const _);

    if OPEN_FILES.lock().unwrap().contains_key(&local_fd) {
        local_fd
    } else {
        libc::fileno(file_stream)
    }
}

/// Hook for `libc::lseek`.
///
/// **Bypassed** by `fd`s that are not managed by us (not found in `OPEN_FILES`).
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

        match lseek_result {
            Ok(offset) => offset,
            Err(fail) => {
                error!("Failed seeking file with {:#?}", fail);
                fail.into()
            }
        }
    } else {
        libc::lseek(fd, offset, whence)
    }
}

/// Hook for `libc::write`.
///
/// **Bypassed** by `fd`s that are not managed by us (not found in `OPEN_FILES`).
pub(crate) unsafe extern "C" fn write_detour(
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

        match write_result {
            Ok(write_amount) => write_amount.try_into().unwrap(),
            Err(fail) => {
                error!("Failed writing file with {:#?}", fail);
                -1
            }
        }
    } else {
        libc::write(fd, buffer, count)
    }
}

/// Convenience function to setup file hooks (`x_detour`) with `frida_gum`.
pub(crate) fn enable_file_hooks(interceptor: &mut Interceptor) {
    hook!(interceptor, "open", open_detour);
    hook!(interceptor, "openat", openat_detour);
    hook!(interceptor, "fopen", fopen_detour);
    hook!(interceptor, "fdopen", fdopen_detour);
    hook!(interceptor, "read", read_detour);
    hook!(interceptor, "fread", fread_detour);
    hook!(interceptor, "fileno", fileno_detour);
    hook!(interceptor, "lseek", lseek_detour);
    hook!(interceptor, "write", write_detour);
}
