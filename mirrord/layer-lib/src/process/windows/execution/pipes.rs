//! Pipe management utilities for Windows process execution.
//!
//! This module provides utilities for creating and managing pipes for stdout/stderr
//! redirection in Windows processes.

use std::{ptr, thread};

use winapi::{
    shared::{
        ntdef::{HANDLE, NULL},
        winerror::{ERROR_BROKEN_PIPE, ERROR_NO_DATA},
    },
    um::{
        errhandlingapi::GetLastError,
        fileapi::{ReadFile, WriteFile},
        handleapi::{CloseHandle, SetHandleInformation},
        namedpipeapi::CreatePipe,
        processthreadsapi::STARTUPINFOW,
        winbase::{HANDLE_FLAG_INHERIT, STARTF_USESTDHANDLES},
    },
};

use crate::error::{LayerError, LayerResult};

/// A managed pipe that handles creation, inheritance, forwarding and cleanup
pub struct PipeForwarder {
    read_handle: HANDLE,
    write_handle: HANDLE,
    forwarding_thread: Option<thread::JoinHandle<()>>,
}

impl PipeForwarder {
    /// Create anonymous pipe pair with proper inheritance setup
    pub fn new_anonymous(name: &str) -> LayerResult<Self> {
        unsafe {
            let mut read_handle: HANDLE = NULL;
            let mut write_handle: HANDLE = NULL;

            // Create the pipe
            if CreatePipe(&mut read_handle, &mut write_handle, ptr::null_mut(), 0) == 0 {
                return Err(LayerError::IO(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Failed to create pipe for {}", name),
                )));
            }

            // Disable inheritance for read handle (parent side)
            if SetHandleInformation(read_handle, HANDLE_FLAG_INHERIT, 0) == 0 {
                CloseHandle(read_handle);
                CloseHandle(write_handle);
                return Err(LayerError::IO(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Failed to set inheritance for {} read handle", name),
                )));
            }

            Ok(Self {
                read_handle,
                write_handle,
                forwarding_thread: None,
            })
        }
    }

    /// Get write handle for child process
    pub fn write_handle(&self) -> HANDLE {
        self.write_handle
    }

    /// Get read handle for parent process  
    pub fn read_handle(&self) -> HANDLE {
        self.read_handle
    }

    /// Start background forwarding from read handle to destination
    pub fn start_forwarding(&mut self, destination: HANDLE) -> LayerResult<()> {
        if self.forwarding_thread.is_some() {
            return Err(LayerError::IO(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "Forwarding already started".to_string(),
            )));
        }

        let src_handle = self.read_handle as usize;
        let dst_handle = destination as usize;

        let handle = thread::spawn(move || {
            let mut buffer = vec![0u8; 8192];
            let mut bytes_read = 0u32;
            let mut bytes_written = 0u32;

            loop {
                unsafe {
                    let read_success = ReadFile(
                        src_handle as HANDLE,
                        buffer.as_mut_ptr() as *mut _,
                        buffer.len() as u32,
                        &mut bytes_read,
                        ptr::null_mut(),
                    );

                    // Check for actual read failure
                    if read_success == 0 {
                        let error = GetLastError();
                        match error {
                            ERROR_BROKEN_PIPE => {
                                tracing::debug!(
                                    "📡 Pipe closed (broken pipe), forwarding thread exiting"
                                );
                                break;
                            }
                            ERROR_NO_DATA => {
                                tracing::debug!("📡 No data available, continuing...");
                                std::thread::sleep(std::time::Duration::from_millis(10));
                                continue;
                            }
                            _ => {
                                tracing::debug!(
                                    "📡 ReadFile failed with error {}, forwarding thread exiting",
                                    error
                                );
                                break;
                            }
                        }
                    }

                    // Handle case where ReadFile succeeded but no bytes were read
                    if bytes_read == 0 {
                        tracing::debug!("📡 ReadFile succeeded but 0 bytes read, continuing...");
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }

                    tracing::debug!("📡 Read {} bytes, forwarding...", bytes_read);

                    let write_success = WriteFile(
                        dst_handle as HANDLE,
                        buffer.as_ptr() as *const _,
                        bytes_read,
                        &mut bytes_written,
                        ptr::null_mut(),
                    );

                    if write_success == 0 {
                        tracing::debug!("📡 Write failed, forwarding thread exiting");
                        break;
                    }

                    tracing::debug!("📡 Successfully wrote {} bytes", bytes_written);
                }
            }
        });

        self.forwarding_thread = Some(handle);
        Ok(())
    }

    /// Stop forwarding and wait for thread completion
    pub fn stop_forwarding(&mut self) {
        if let Some(handle) = self.forwarding_thread.take() {
            unsafe {
                CloseHandle(self.read_handle);
                self.read_handle = NULL;
            }

            let _ = handle.join();
        }
    }

    /// Detach forwarding thread to continue running independently
    /// This allows the forwarding to continue even after this object is dropped
    pub fn detach_forwarding(&mut self) {
        if let Some(handle) = self.forwarding_thread.take() {
            // Don't join the thread, let it continue running independently
            // The thread will naturally exit when the child process closes its end of the pipe
            std::mem::forget(handle);

            // Don't close handles here - let the child process continue using them
            // The handles will be cleaned up when the child process exits
        }
    }
}

impl Drop for PipeForwarder {
    fn drop(&mut self) {
        self.stop_forwarding();

        unsafe {
            if self.read_handle != NULL {
                CloseHandle(self.read_handle);
            }
            if self.write_handle != NULL {
                CloseHandle(self.write_handle);
            }
        }
    }
}

/// Manages the three pipe forwarders for stdio redirection
pub struct StdioRedirection {
    pub stdout: Option<PipeForwarder>,
    pub stderr: Option<PipeForwarder>,
    // Future expansion
    pub stdin: Option<PipeForwarder>,
}

impl StdioRedirection {
    /// Create stdio redirection with stdout/stderr pipes
    pub fn create_for_child() -> LayerResult<Self> {
        Ok(Self {
            stdout: Some(PipeForwarder::new_anonymous("stdout")?),
            stderr: Some(PipeForwarder::new_anonymous("stderr")?),
            stdin: None, // Future expansion
        })
    }

    /// Apply operation to both stdout and stderr pipes
    fn apply_to_pipes_void<F>(&mut self, mut operation: F)
    where
        F: FnMut(&mut PipeForwarder),
    {
        if let Some(ref mut stdout) = self.stdout {
            operation(stdout);
        }
        if let Some(ref mut stderr) = self.stderr {
            operation(stderr);
        }
    }

    /// Setup startup info with pipe handles for child process
    pub fn setup_startup_info(&self, startup_info: &mut STARTUPINFOW) {
        startup_info.dwFlags |= STARTF_USESTDHANDLES;

        if let Some(ref stdout) = self.stdout {
            startup_info.hStdOutput = stdout.write_handle();
        }
        if let Some(ref stderr) = self.stderr {
            startup_info.hStdError = stderr.write_handle();
        }
    }

    /// Start forwarding stdout/stderr to parent handles
    pub fn start_forwarding(
        &mut self,
        parent_stdout: HANDLE,
        parent_stderr: HANDLE,
    ) -> LayerResult<()> {
        tracing::debug!(
            "🎯 StdioRedirection::start_forwarding called with stdout: {:?}, stderr: {:?}",
            parent_stdout,
            parent_stderr
        );

        if let Some(ref mut stdout) = self.stdout {
            tracing::debug!("🎯 Starting stdout forwarding...");
            stdout.start_forwarding(parent_stdout)?;
        }
        if let Some(ref mut stderr) = self.stderr {
            tracing::debug!("🎯 Starting stderr forwarding...");
            stderr.start_forwarding(parent_stderr)?;
        }
        Ok(())
    }

    /// Stop all forwarding
    pub fn stop_forwarding(&mut self) {
        self.apply_to_pipes_void(|pipe| pipe.stop_forwarding());
    }

    /// Detach all forwarding threads so they continue running independently
    pub fn detach_forwarding(&mut self) {
        self.apply_to_pipes_void(|pipe| pipe.detach_forwarding());
    }

    /// Close write handles after process creation
    pub fn close_child_handles(&mut self) {
        unsafe {
            self.apply_to_pipes_void(|pipe| {
                CloseHandle(pipe.write_handle());
            });
        }
    }
}

impl Drop for StdioRedirection {
    fn drop(&mut self) {
        self.stop_forwarding();
    }
}
