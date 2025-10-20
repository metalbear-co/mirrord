//! Pipe management utilities for Windows process execution.
//!
//! This module provides utilities for creating and managing pipes for stdout/stderr
//! redirection in Windows processes.

use std::{ptr, thread};

use winapi::{
    shared::ntdef::{HANDLE, NULL},
    um::{
        handleapi::{SetHandleInformation, CloseHandle}, 
        namedpipeapi::CreatePipe, 
        winbase::HANDLE_FLAG_INHERIT,
        fileapi::{ReadFile, WriteFile},
        processthreadsapi::STARTUPINFOW,
        winbase::STARTF_USESTDHANDLES,
    },
};

use crate::error::{LayerError, LayerResult};

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

/// A managed pipe that handles creation, inheritance, forwarding and cleanup
pub struct PipeForwarder {
    read_handle: HANDLE,
    write_handle: HANDLE,
    forwarding_thread: Option<thread::JoinHandle<()>>,
    name: String,
}

impl PipeForwarder {
    /// Create new anonymous pipe pair with proper inheritance setup
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
                name: name.to_string(),
            })
        }
    }

    /// Get the write handle for child process
    pub fn write_handle(&self) -> HANDLE {
        self.write_handle
    }

    /// Get the read handle for parent process  
    pub fn read_handle(&self) -> HANDLE {
        self.read_handle
    }

    /// Start forwarding from read handle to destination in background thread
    pub fn start_forwarding(&mut self, destination: HANDLE, config: PipeConfig) -> LayerResult<()> {
        if self.forwarding_thread.is_some() {
            return Err(LayerError::IO(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("Forwarding already started for {}", self.name),
            )));
        }

        let src_handle = self.read_handle as usize;
        let dst_handle = destination as usize;

        let handle = thread::spawn(move || {
            let mut buffer = vec![0u8; config.buffer_size];
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

                    if read_success == 0 || bytes_read == 0 {
                        break;
                    }

                    let write_success = WriteFile(
                        dst_handle as HANDLE,
                        buffer.as_ptr() as *const _,
                        bytes_read,
                        &mut bytes_written,
                        ptr::null_mut(),
                    );

                    if write_success == 0 {
                        break;
                    }
                }
            }
        });

        self.forwarding_thread = Some(handle);
        Ok(())
    }

    /// Stop forwarding and wait for thread completion
    pub fn stop_forwarding(&mut self) -> LayerResult<()> {
        if let Some(handle) = self.forwarding_thread.take() {
            // Close read handle to signal thread to stop
            unsafe {
                CloseHandle(self.read_handle);
                self.read_handle = NULL;
            }
            
            // Wait for thread to finish (with timeout)
            let _ = handle.join(); // Best effort - ignore errors
        }
        Ok(())
    }

    /// Check if forwarding is active
    pub fn is_forwarding(&self) -> bool {
        self.forwarding_thread.is_some()
    }
}

impl Drop for PipeForwarder {
    fn drop(&mut self) {
        // Stop forwarding thread gracefully
        let _ = self.stop_forwarding();
        
        // Close remaining handles
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
    pub stdin: Option<PipeForwarder>,  // Future expansion
}

impl StdioRedirection {
    /// Create stdio redirection with stdout/stderr forwarders
    pub fn create_for_child() -> LayerResult<Self> {
        Ok(Self {
            stdout: Some(PipeForwarder::new_anonymous("stdout")?),
            stderr: Some(PipeForwarder::new_anonymous("stderr")?),
            stdin: None, // Not needed currently
        })
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
    pub fn start_forwarding(&mut self, parent_stdout: HANDLE, parent_stderr: HANDLE) -> LayerResult<()> {
        if let Some(ref mut stdout) = self.stdout {
            stdout.start_forwarding(parent_stdout, PipeConfig::default())?;
        }
        if let Some(ref mut stderr) = self.stderr {
            stderr.start_forwarding(parent_stderr, PipeConfig::default())?;
        }
        Ok(())
    }
    
    /// Stop all forwarding
    pub fn stop_forwarding(&mut self) -> LayerResult<()> {
        if let Some(ref mut stdout) = self.stdout {
            stdout.stop_forwarding()?;
        }
        if let Some(ref mut stderr) = self.stderr {
            stderr.stop_forwarding()?;
        }
        Ok(())
    }

    /// Close write handles after process creation (child has inherited them)
    pub fn close_child_handles(&mut self) {
        unsafe {
            if let Some(ref stdout) = self.stdout {
                CloseHandle(stdout.write_handle());
            }
            if let Some(ref stderr) = self.stderr {
                CloseHandle(stderr.write_handle());
            }
        }
    }
}

impl Drop for StdioRedirection {
    fn drop(&mut self) {
        let _ = self.stop_forwarding(); // Best effort cleanup
    }
}
