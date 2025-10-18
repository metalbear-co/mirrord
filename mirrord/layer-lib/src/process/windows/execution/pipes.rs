//! Pipe management utilities for Windows process execution.
//!
//! This module provides utilities for creating and managing pipes for stdout/stderr
//! redirection in Windows processes.

use std::ptr;

use winapi::{
    shared::ntdef::{HANDLE, NULL},
    um::{handleapi::SetHandleInformation, namedpipeapi::CreatePipe, winbase::HANDLE_FLAG_INHERIT},
};

use crate::error::{LayerError, LayerResult};

/// Pipe handles for stdout/stderr redirection
#[derive(Debug)]
pub struct PipeHandles {
    pub stdout_read: HANDLE,
    pub stderr_read: HANDLE,
}

/// Create pipes for stdout and stderr redirection
pub fn create_stdout_stderr_pipes() -> LayerResult<(PipeHandles, HANDLE, HANDLE)> {
    unsafe {
        let mut stdout_read: HANDLE = NULL;
        let mut stdout_write: HANDLE = NULL;
        let mut stderr_read: HANDLE = NULL;
        let mut stderr_write: HANDLE = NULL;

        let pipes_created = CreatePipe(&mut stdout_read, &mut stdout_write, ptr::null_mut(), 0)
            != 0
            && CreatePipe(&mut stderr_read, &mut stderr_write, ptr::null_mut(), 0) != 0
            && SetHandleInformation(stdout_read, HANDLE_FLAG_INHERIT, 0) != 0
            && SetHandleInformation(stderr_read, HANDLE_FLAG_INHERIT, 0) != 0;

        if !pipes_created {
            return Err(LayerError::IO(std::io::Error::new(
                std::io::ErrorKind::Other,
                "Failed to create pipes for child process stdout/stderr redirection",
            )));
        }

        let pipe_handles = PipeHandles {
            stdout_read,
            stderr_read,
        };

        Ok((pipe_handles, stdout_write, stderr_write))
    }
}

/// Setup pipe handles for process startup info
pub fn setup_pipe_handles_for_startup(
    startup_info: &mut winapi::um::processthreadsapi::STARTUPINFOW,
    stdout_write: HANDLE,
    stderr_write: HANDLE,
) {
    use winapi::um::winbase::STARTF_USESTDHANDLES;

    startup_info.dwFlags |= STARTF_USESTDHANDLES;
    startup_info.hStdOutput = stdout_write;
    startup_info.hStdError = stderr_write;
}

/// Configuration for pipe forwarding operations
#[derive(Debug, Clone)]
pub struct PipeConfig {
    pub buffer_size: usize,
    pub timeout_ms: u32,
}

impl Default for PipeConfig {
    fn default() -> Self {
        Self {
            buffer_size: 8192,
            timeout_ms: 1000,
        }
    }
}

/// Forward data from one pipe handle to another in a background thread
/// 
/// This function creates a background thread that continuously reads from the
/// source handle and writes to the destination handle.
pub fn forward_pipe_raw_handle_threaded(
    src_handle: usize,
    dst_handle: usize,
    _config: PipeConfig,
) {
    use std::thread;
    use winapi::um::fileapi::{ReadFile, WriteFile};
    
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        let mut bytes_read = 0u32;
        let mut bytes_written = 0u32;
        
        loop {
            unsafe {
                let read_success = ReadFile(
                    src_handle as HANDLE,
                    buffer.as_mut_ptr() as *mut _,
                    buffer.len() as u32,
                    &mut bytes_read,
                    std::ptr::null_mut(),
                );
                
                if read_success == 0 || bytes_read == 0 {
                    break;
                }
                
                let write_success = WriteFile(
                    dst_handle as HANDLE,
                    buffer.as_ptr() as *const _,
                    bytes_read,
                    &mut bytes_written,
                    std::ptr::null_mut(),
                );
                
                if write_success == 0 {
                    break;
                }
            }
        }
    });
}
