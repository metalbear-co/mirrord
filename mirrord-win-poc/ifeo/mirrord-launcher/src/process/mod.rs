//! Simple utilities for interacting with processes.

use std::{io::Result, mem::MaybeUninit, path::Path};

use dll_syringe::{Syringe, process::OwnedProcess};
use mirrord_win_str::string_to_u16_buffer;
use winapi::um::{
    processthreadsapi::{CreateProcessW, PROCESS_INFORMATION, ResumeThread, STARTUPINFOW},
    winbase::CREATE_SUSPENDED,
    winnt::HANDLE,
};

use crate::handle::handle::SafeHandle;

/// Suspended state.
#[derive(Debug, PartialEq, Eq)]
pub enum Suspended {
    Yes,
    No,
}

/// Structure containing the [`SafeHandle`]-s returned by
/// [`CreateProcessW`]. Moreover, contains process and thread ID.
#[derive(Debug)]
pub struct CreateProcessInformation {
    pub process: SafeHandle,
    pub thread: SafeHandle,
    pub process_id: u32,
    pub thread_id: u32,
}

/// Create a process found at path, with the specified arguments.
/// May be suspended depending on the arguments. Resuming required.
///
/// # Arguments
///
/// * `path` - Path to process to be started, relative/absolute.
/// * `args` - Arguments to be passed to process creation.
/// * `suspended` - Whether the process should start suspended or not.
pub fn create_process<T: AsRef<Path>, U: AsRef<[String]>>(
    path: T,
    args: U,
    suspended: Suspended,
) -> Option<CreateProcessInformation> {
    let path = path.as_ref();
    let args = args.as_ref();

    // Path conversion.
    let path = path.to_str()?;
    let os_path = string_to_u16_buffer(path);

    let command_line = format!("\"{}\" {}", path, args.join(" "));

    // Must reserve enough space for `out`.
    let mut os_command_line: Vec<u16> = vec![0u16; 1024];
    let to_os_command_line = string_to_u16_buffer(command_line);
    os_command_line[..to_os_command_line.len()].copy_from_slice(&to_os_command_line);

    // Set flags.
    let mut flags = 0u32;
    if suspended == Suspended::Yes {
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

    Some(CreateProcessInformation {
        process: SafeHandle::from(process_info.hProcess),
        thread: SafeHandle::from(process_info.hThread),
        process_id: process_info.dwProcessId,
        thread_id: process_info.dwThreadId,
    })
}

/// Resume possibly suspended thread by [`HANDLE`]. Returns operation result.
///
/// # Arguments
///
/// * `thread` - [`HANDLE`] to possibly suspended thread.
pub fn resume_thread(thread: HANDLE) -> bool {
    let ret = unsafe { ResumeThread(thread) };
    ret != u32::MAX
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

/// Return absolute, resolved ("canonicalized") path from any path.
///
/// # Arguments
///
/// * `path` - Path to canonicalize.
pub fn absolute_path<T: AsRef<Path>>(path: T) -> Option<String> {
    let path = path.as_ref();

    Some(std::fs::canonicalize(path).ok()?.to_str()?.to_string())
}

pub fn inject_dll<T: AsRef<Path>>(pid: u32, dll: T) -> Result<()> {
    let process = OwnedProcess::from_pid(pid)?;
    let syringe = Syringe::for_process(process);
    let _ = syringe.inject(dll).unwrap();
    Ok(())
}

#[cfg(test)]
mod tests;
