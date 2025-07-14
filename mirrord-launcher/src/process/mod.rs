//! Simple utilities for interacting with processes.

use std::{mem::MaybeUninit, path::Path};

use winapi::{
    shared::winerror::ERROR_SUCCESS,
    um::{
        errhandlingapi::GetLastError, processthreadsapi::{CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW}, winbase::CREATE_SUSPENDED
    },
};

use crate::{
    handle::handle::SafeHandle,
    win_str::{string_to_u16_buffer, u16_buffer_to_string},
};

/// Structure containing the [`SafeHandle`]-s returned by
/// [`CreateProcessW`]. Because RAII, you must wait on the handles,
/// if you plan to wait out the process.
#[derive(Debug)]
pub struct CreateProcessHandles {
    process: SafeHandle,
    thread: SafeHandle,
}

pub fn create_process<T: AsRef<Path>, U: AsRef<[String]>>(
    path: T,
    args: U,
    suspended: bool,
) -> Option<CreateProcessHandles> {
    let path = path.as_ref();
    let args = args.as_ref();

    // Path conversion.
    let path = path.to_str()?;
    let os_path = string_to_u16_buffer(path);

    let command_line = format!("\"{}\" {}", path, args.join(" "));

    // Must reserve enough space for `out`.
    let mut os_command_line: Vec<u16> = vec![0u16; 1024];
    os_command_line.extend(string_to_u16_buffer(command_line));

    // Set flags.
    let mut flags = 0u32;
    if suspended {
        flags |= CREATE_SUSPENDED;
    }

    // Out.
    let mut startup_info: STARTUPINFOW =
        unsafe { MaybeUninit::<STARTUPINFOW>::zeroed().assume_init() };
    let mut process_info: PROCESS_INFORMATION =
        unsafe { MaybeUninit::<PROCESS_INFORMATION>::zeroed().assume_init() };

    let ret = unsafe {
        CreateProcessW(
            os_path.as_ptr(),
            os_command_line.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            flags,
            std::ptr::null_mut(),
            std::ptr::null(),
            &mut startup_info,
            &mut process_info,
        )
    };

    if ret == 0 {
        return None;
    }

    Some(CreateProcessHandles {
        process: SafeHandle::from(process_info.hProcess),
        thread: SafeHandle::from(process_info.hThread),
    })
}

/// Retrieves process name (file name) from path, if there is a file there.
///
/// # Arguments
///
/// * `path` - Path to extract process name (file name) from.
pub fn process_name_from_path<T: AsRef<Path>>(path: T) -> Option<String> {
    let path = path.as_ref();

    if std::fs::metadata(path).ok()?.is_file() {
        let file_name = path.file_name()?;
        return Some(file_name.to_str()?.to_string());
    }

    None
}

#[cfg(test)]
mod tests;
