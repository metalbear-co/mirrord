use std::{
    fs::File,
    os::windows::io::{FromRawHandle, RawHandle},
    time::Duration,
};

use libc::EXIT_FAILURE;
use mirrord_layer_lib::process::pipe::{PipeConfig, spawn_stdio_forwarders};
use windows::Win32::{
    Foundation::HANDLE,
    System::Threading::{self as Win32Threading, ResumeThread, WaitForSingleObject},
};

pub struct HandleWrapper(pub HANDLE);

impl From<HandleWrapper> for File {
    fn from(handle_wrapper: HandleWrapper) -> Self {
        unsafe {
            // SAFETY: Caller guarantees the HANDLE is valid and suitable for use as a stdio stream.
            let raw_handle = handle_wrapper.0;
            File::from_raw_handle(raw_handle.0 as RawHandle)
        }
    }
}

#[derive(Debug)]
pub struct WindowsProcess {
    pub process_info: Win32Threading::PROCESS_INFORMATION,
    pub stdin: File,
    pub stdout: File,
    pub stderr: File,
}

impl WindowsProcess {
    pub async fn join_std_pipes(mut self) -> Result<i32, Box<dyn std::error::Error>> {
        // Extract what we need while avoiding the Drop trait issue
        let process_info = self.process_info;
        let stdout = std::mem::replace(&mut self.stdout, unsafe {
            File::from_raw_handle(std::ptr::null_mut())
        });
        let stderr = std::mem::replace(&mut self.stderr, unsafe {
            File::from_raw_handle(std::ptr::null_mut())
        });
        let stdin = std::mem::replace(&mut self.stdin, unsafe {
            File::from_raw_handle(std::ptr::null_mut())
        });

        // Mark handles as consumed to prevent double-cleanup in Drop
        self.process_info.hProcess = HANDLE(std::ptr::null_mut());
        self.process_info.hThread = HANDLE(std::ptr::null_mut());

        // Use the common pipe utilities for stdio forwarding
        let config = PipeConfig::default();
        let (stdout_handle, stderr_handle, stdin_handle) =
            spawn_stdio_forwarders(stdout, stderr, stdin, config).await;

        // Wait for the process to complete asynchronously
        let exit_code = tokio::task::spawn_blocking({
            let process_handle_raw = process_info.hProcess.0 as usize;
            move || {
                unsafe {
                    let process_handle = HANDLE(process_handle_raw as *mut std::ffi::c_void);
                    WaitForSingleObject(process_handle, u32::MAX);

                    // Get exit code
                    let mut exit_code: u32 = 0;
                    if windows::Win32::System::Threading::GetExitCodeProcess(
                        process_handle,
                        &mut exit_code,
                    )
                    .is_ok()
                    {
                        exit_code as i32
                    } else {
                        EXIT_FAILURE
                    }
                }
            }
        })
        .await
        .unwrap_or(EXIT_FAILURE);

        // Wait for I/O tasks to complete
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            let _ = stdout_handle.await;
            let _ = stderr_handle.await;
            let _ = stdin_handle.await;
        })
        .await;

        // Manually clean up process handles
        unsafe {
            let _ = ::windows::Win32::Foundation::CloseHandle(process_info.hProcess);
            let _ = ::windows::Win32::Foundation::CloseHandle(process_info.hThread);
        }

        Ok(exit_code)
    }
}

impl Drop for WindowsProcess {
    fn drop(&mut self) {
        // Only clean up if we still have valid handles
        // join_std_pipes will manually clean up if it takes ownership
        if self.process_info.hProcess.0 != std::ptr::null_mut() {
            unsafe {
                let _ = ::windows::Win32::Foundation::CloseHandle(self.process_info.hProcess);
                let _ = ::windows::Win32::Foundation::CloseHandle(self.process_info.hThread);
            }
        }
    }
}

pub trait WindowsProcessExtSuspended {
    fn resume(&self) -> windows::core::Result<()>;
}

impl WindowsProcessExtSuspended for WindowsProcess {
    fn resume(&self) -> windows::core::Result<()> {
        unsafe {
            // ResumeThread: If the function fails, the return value is (DWORD) -1
            if ResumeThread(self.process_info.hThread) == u32::MAX {
                return Err(windows::core::Error::from_win32());
            }
        }
        Ok(())
    }
}
