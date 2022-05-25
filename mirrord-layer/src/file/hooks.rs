use std::{ffi::CStr, io::SeekFrom, os::unix::io::RawFd, path::PathBuf, ptr, slice};

use frida_gum::{interceptor::Interceptor, Module, NativePointer};
use libc::{self, c_char, c_int, c_void, off_t, size_t, ssize_t, AT_FDCWD, FILE};
use tracing::{debug, error};

use super::{IGNORE_FILES, OPEN_FILES};
use crate::{
    file::{
        ops::{lseek, open, read, write},
        ReadFile,
    },
    HOOK_SENDER,
};

/// NOTE(alex): libc also has an `open64` function. Both functions point to the same address, so
/// trying to intercept them all will result in an `InterceptorAlreadyReplaced` error.
pub(super) unsafe extern "C" fn open_detour(raw_path: *const c_char, open_flags: c_int) -> RawFd {
    let path: PathBuf = CStr::from_ptr(raw_path)
        .to_str()
        .expect("Failed converting path from c_char!")
        .into();

    if IGNORE_FILES.is_match(path.to_str().unwrap()) {
        let bypassed_fd = libc::open(raw_path, open_flags);
        debug!("open_detour -> bypassed_fd {bypassed_fd:#?}");

        bypassed_fd
    } else {
        let sender = HOOK_SENDER.as_ref().unwrap();
        let open_result = open(sender, path, open_flags);

        open_result
            .map_err(|fail| {
                error!("Failed opening file with {fail:#?}");
                libc::EFAULT
            })
            .unwrap_or_else(|fail| fail)
    }
}

pub(super) unsafe extern "C" fn fopen_detour(
    raw_path: *const c_char,
    raw_mode: *const c_char,
) -> *mut FILE {
    let path: PathBuf = CStr::from_ptr(raw_path)
        .to_str()
        .expect("Failed converting path from c_char!")
        .into();

    let mode: PathBuf = CStr::from_ptr(raw_path)
        .to_str()
        .expect("Failed converting mode from c_char!")
        .into();

    if IGNORE_FILES.is_match(path.to_str().unwrap()) {
        let bypassed_file = libc::fopen(raw_path, raw_mode);
        debug!("fopen_detour -> bypassed_file {bypassed_file:#?}");

        bypassed_file
    } else {
        todo!()
    }
}

// TODO(alex) [mid] 2022-05-25: Implement this? It has some tricky parts around relative
// directories.
pub(super) unsafe extern "C" fn openat_detour(
    fd: RawFd,
    raw_path: *const c_char,
    open_flags: c_int,
) -> RawFd {
    let path: PathBuf = CStr::from_ptr(raw_path)
        .to_str()
        .expect("Failed converting path from c_char!")
        .into();

    if IGNORE_FILES.is_match(path.to_str().unwrap()) {
        let bypassed_fd = libc::openat(fd, raw_path, open_flags);

        bypassed_fd
    } else {
        let sender = HOOK_SENDER.as_ref().unwrap();

        // NOTE(alex): `openat` docs talk about this special case where `fd` is set as `AT_FDCWD`,
        // when this happens `openat` behaves exactly like `open`.
        let open_result = if fd & AT_FDCWD != 0 {
            open(sender, path, open_flags)
        } else {
            todo!()
        };

        open_result
            .map_err(|fail| {
                error!("Failed opening file with {fail:#?}");
                libc::EFAULT
            })
            .unwrap_or_else(|fail| fail)
    }
}

pub(crate) unsafe extern "C" fn read_detour(fd: RawFd, buf: *mut c_void, count: size_t) -> ssize_t {
    // NOTE(alex): We're only interested in files that are handled by `mirrord-agent`.
    let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).cloned();
    if let Some(remote_fd) = remote_fd {
        debug!("read_detour -> managed_fd {remote_fd:#?} | fd {fd:#?} | count {count:#?}");

        let sender = HOOK_SENDER.as_ref().unwrap();
        let read_result = read(sender, remote_fd, count).map(|read_file| {
            let ReadFile { bytes, read_amount } = read_file;

            // NOTE(alex): There is no distinction between reading 0 bytes or if we hit EOF, but we
            // only copy to buffer if we have something to copy.
            if read_amount > 0 {
                let read_ptr = bytes.as_ptr();
                let out_buffer = buf.cast();
                ptr::copy(read_ptr, out_buffer, read_amount);
            }

            // WARN(alex): Must be careful when it comes to `EOF`, incorrect handling
            // appears as the `read` call being repeated.
            read_amount.try_into().unwrap()
        });

        read_result
            .map_err(|fail| {
                error!("Failed reading file with {fail:#?}");
                -1
            })
            .unwrap_or_else(|fail| fail)
    } else {
        let read_count = libc::read(fd, buf, count);
        read_count
    }
}

pub(crate) unsafe extern "C" fn lseek_detour(fd: RawFd, offset: off_t, whence: c_int) -> off_t {
    let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).cloned();
    if let Some(remote_fd) = remote_fd {
        debug!(
            "lseek_detour -> managed_fd {remote_fd:#?} | offset {offset:#?} | whence {whence:#?}"
        );

        let seek_from = match whence {
            libc::SEEK_SET => SeekFrom::Start(offset as u64),
            libc::SEEK_CUR => SeekFrom::Current(offset),
            libc::SEEK_END => SeekFrom::End(offset),
            libc::SEEK_DATA => todo!(),
            libc::SEEK_HOLE => todo!(),
            _other => {
                error!("lseek_detour -> invalid value for whence {whence:#?}");
                return libc::EINVAL.into();
            }
        };

        let sender = HOOK_SENDER.as_ref().unwrap();
        let lseek_result =
            lseek(sender, remote_fd, seek_from).map(|offset| offset.try_into().unwrap());

        lseek_result
            .map_err(|fail| {
                error!("Failed lseek operation with {fail:#?}");
                libc::EFAULT.into()
            })
            .unwrap_or_else(|fail| fail)
    } else {
        let result_offset = libc::lseek(fd, offset, whence);
        result_offset
    }
}

pub(crate) unsafe extern "C" fn write_detour(
    fd: RawFd,
    buf: *const c_void,
    count: size_t,
) -> ssize_t {
    let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).cloned();
    if let Some(remote_fd) = remote_fd {
        debug!("write_detour -> managed_fd {remote_fd:#?} | count {count:#?}");

        if buf.is_null() {
            return libc::EFAULT.try_into().unwrap();
        }

        // WARN(alex): Be veeery careful here, you cannot construct the `Vec` directly, as the
        // buffer allocation is handled on the C side.
        let outside_buffer = slice::from_raw_parts(buf as *const u8, count);
        let write_bytes = outside_buffer.to_vec();

        let sender = HOOK_SENDER.as_ref().unwrap();
        let write_result = write(sender, remote_fd, write_bytes);

        write_result
            .map_err(|fail| {
                error!("Failed writing file with {fail:#?}");
                -1 as isize
            })
            .unwrap_or_else(|fail| fail)
    } else {
        let written_amount = libc::write(fd, buf, count);
        written_amount
    }
}

pub(crate) fn enable_file_hooks(interceptor: &mut Interceptor) {
    interceptor
        .replace(
            Module::find_export_by_name(None, "open").unwrap(),
            NativePointer(open_detour as *mut c_void),
            NativePointer(std::ptr::null_mut()),
        )
        .unwrap();

    interceptor
        .replace(
            Module::find_export_by_name(None, "fopen").unwrap(),
            NativePointer(fopen_detour as *mut c_void),
            NativePointer(std::ptr::null_mut()),
        )
        .unwrap();

    interceptor
        .replace(
            Module::find_export_by_name(None, "read").unwrap(),
            NativePointer(read_detour as *mut c_void),
            NativePointer(std::ptr::null_mut()),
        )
        .unwrap();

    interceptor
        .replace(
            Module::find_export_by_name(None, "lseek").unwrap(),
            NativePointer(lseek_detour as *mut c_void),
            NativePointer(std::ptr::null_mut()),
        )
        .unwrap();

    interceptor
        .replace(
            Module::find_export_by_name(None, "write").unwrap(),
            NativePointer(write_detour as *mut c_void),
            NativePointer(std::ptr::null_mut()),
        )
        .unwrap();
}
