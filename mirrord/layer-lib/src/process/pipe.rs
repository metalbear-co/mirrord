//! Process I/O pipe utilities for cross-platform process communication.
//!
//! This module provides functionality for forwarding pipes between processes,
//! supporting both synchronous and asynchronous operations. Currently implemented
//! for Windows, but designed to be extensible for Unix platforms.

use std::io::{self, Read, Write};
#[cfg(windows)]
use std::ptr;

#[cfg(windows)]
use winapi::{
    shared::ntdef::HANDLE,
    um::{
        fileapi::{ReadFile, WriteFile},
        handleapi::CloseHandle,
    },
};

/// Configuration for pipe forwarding operations
#[derive(Debug, Clone)]
pub struct PipeConfig {
    /// Buffer size for pipe operations (default: 4096 bytes)
    pub buffer_size: usize,
    /// Whether to flush output after each write
    pub auto_flush: bool,
}

impl Default for PipeConfig {
    fn default() -> Self {
        Self {
            buffer_size: 4096,
            auto_flush: true,
        }
    }
}

/// Generic pipe forwarder that works with any `Read` and `Write` implementations
pub fn forward_pipe<R: Read, W: Write>(
    mut reader: R,
    mut writer: W,
    config: PipeConfig,
) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0u8; config.buffer_size];
    let mut all_data = Vec::new();

    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break, // EOF
            Ok(n) => {
                let data = &buffer[..n];
                all_data.extend_from_slice(data);

                writer.write_all(data)?;
                if config.auto_flush {
                    writer.flush()?;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }

    Ok(all_data)
}

/// Convenience function for forwarding pipes without collecting output
pub fn forward_pipe_simple<R: Read, W: Write>(
    reader: R,
    writer: W,
    config: PipeConfig,
) -> io::Result<()> {
    forward_pipe(reader, writer, config).map(|_| ())
}

#[cfg(windows)]
/// Windows-specific pipe forwarder using raw HANDLE types for maximum compatibility
/// with existing Windows layer code that uses winapi directly.
pub fn forward_pipe_raw_handle(
    child_handle: HANDLE,
    parent_handle: HANDLE,
    config: PipeConfig,
) -> io::Result<()> {
    let mut buffer = vec![0u8; config.buffer_size];
    let mut bytes_read = 0;
    let mut bytes_written = 0;

    loop {
        let read_success = unsafe {
            ReadFile(
                child_handle,
                buffer.as_mut_ptr() as *mut _,
                buffer.len() as u32,
                &mut bytes_read,
                ptr::null_mut(),
            )
        };

        if read_success == 0 || bytes_read == 0 {
            break; // Pipe closed or read failed
        }

        let write_success = unsafe {
            WriteFile(
                parent_handle,
                buffer.as_ptr() as *const _,
                bytes_read,
                &mut bytes_written,
                ptr::null_mut(),
            )
        };

        if write_success == 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

#[cfg(windows)]
/// Spawns a thread to forward pipes using raw Windows handles
pub fn forward_pipe_raw_handle_threaded(
    child_handle_raw: usize,
    parent_handle_raw: usize,
    config: PipeConfig,
) {
    std::thread::spawn(move || {
        let child_handle = child_handle_raw as HANDLE;
        let parent_handle = parent_handle_raw as HANDLE;

        let _ = forward_pipe_raw_handle(child_handle, parent_handle, config);

        // Clean up child handle (parent handle should be managed by caller)
        unsafe {
            CloseHandle(child_handle);
        }
    });
}

/// Async version using tokio for CLI execution
pub async fn forward_pipe_async<R, W>(
    reader: R,
    writer: W,
    config: PipeConfig,
) -> io::Result<Vec<u8>>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    tokio::task::spawn_blocking(move || forward_pipe(reader, writer, config))
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
}

/// Spawn tokio tasks for stdout/stderr/stdin forwarding (CLI pattern)
/// Note: stdin_writer should be the write end of the child process's stdin pipe
pub async fn spawn_stdio_forwarders<R1, R2, W3>(
    stdout_reader: R1,
    stderr_reader: R2,
    stdin_writer: W3,
    config: PipeConfig,
) -> (
    tokio::task::JoinHandle<io::Result<Vec<u8>>>,
    tokio::task::JoinHandle<io::Result<()>>,
    tokio::task::JoinHandle<io::Result<()>>,
)
where
    R1: Read + Send + 'static,
    R2: Read + Send + 'static,
    W3: Write + Send + 'static,
{
    let stdout_config = config.clone();
    let stderr_config = config.clone();
    let stdin_config = config;

    let stdout_handle = tokio::task::spawn_blocking(move || {
        forward_pipe(stdout_reader, io::stdout(), stdout_config)
    });

    let stderr_handle = tokio::task::spawn_blocking(move || {
        forward_pipe_simple(stderr_reader, io::stderr(), stderr_config)
    });

    let stdin_handle = tokio::task::spawn_blocking(move || {
        forward_pipe_simple(io::stdin(), stdin_writer, stdin_config)
    });

    (stdout_handle, stderr_handle, stdin_handle)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn test_forward_pipe_basic() {
        let input = b"Hello, World!";
        let reader = Cursor::new(input);
        let mut writer = Vec::new();

        let config = PipeConfig::default();
        let result = forward_pipe(reader, &mut writer, config).unwrap();

        assert_eq!(result, input);
        assert_eq!(writer, input);
    }

    #[test]
    fn test_forward_pipe_empty() {
        let reader = Cursor::new(b"");
        let mut writer = Vec::new();

        let config = PipeConfig::default();
        let result = forward_pipe(reader, &mut writer, config).unwrap();

        assert!(result.is_empty());
        assert!(writer.is_empty());
    }

    #[test]
    fn test_pipe_config_custom() {
        let config = PipeConfig {
            buffer_size: 1024,
            auto_flush: false,
        };

        assert_eq!(config.buffer_size, 1024);
        assert!(!config.auto_flush);
    }
}
