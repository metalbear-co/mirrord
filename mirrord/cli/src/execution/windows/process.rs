use std::{
    fs::File,
    os::windows::io::{FromRawHandle, RawHandle},
    time::Duration,
};

use windows::Win32::{
    Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0},
    System::Threading::{self as Win32Threading, WaitForSingleObject},
};

pub struct HandleWrapper(pub HANDLE);

impl From<HandleWrapper> for File {
    fn from(handle_wrapper: HandleWrapper) -> Self {
        unsafe {
            // SAFETY: Caller guarantees the HANDLE is valid and suitable for use as a stdio stream.
            let raw_handle = handle_wrapper.0;
            return File::from_raw_handle(raw_handle.0 as RawHandle);
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
    pub async fn join_std_pipes(&mut self) -> windows::core::Result<i32> {
        use std::{
            io::{self, Read, Write},
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
        };

        let process_done = Arc::new(AtomicBool::new(false));

        // Spawn a task to copy stdout
        let mut stdout = std::mem::replace(
            &mut self.stdout,
            File::from(HandleWrapper(INVALID_HANDLE_VALUE)),
        );
        let process_done_stdout = process_done.clone();
        let stdout_handle = tokio::task::spawn_blocking(move || {
            let mut buffer = [0; 1024];
            while !process_done_stdout.load(Ordering::Relaxed) {
                match stdout.read(&mut buffer) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        io::stdout().write_all(&buffer[..n]).ok();
                        io::stdout().flush().ok();
                    }
                    Err(_) => break,
                }
            }
        });

        // Spawn a task to copy stderr
        let mut stderr = std::mem::replace(
            &mut self.stderr,
            File::from(HandleWrapper(INVALID_HANDLE_VALUE)),
        );
        let process_done_stderr = process_done.clone();
        let stderr_handle = tokio::task::spawn_blocking(move || {
            let mut buffer = [0; 1024];
            while !process_done_stderr.load(Ordering::Relaxed) {
                match stderr.read(&mut buffer) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        io::stderr().write_all(&buffer[..n]).ok();
                        io::stderr().flush().ok();
                    }
                    Err(_) => break,
                }
            }
        });

        // Spawn a task to handle stdin
        let mut stdin = std::mem::replace(
            &mut self.stdin,
            File::from(HandleWrapper(INVALID_HANDLE_VALUE)),
        );
        let process_done_stdin = process_done.clone();
        let stdin_handle = tokio::task::spawn_blocking(move || {
            let mut buffer = [0; 1024];
            while !process_done_stdin.load(Ordering::Relaxed) {
                match io::stdin().read(&mut buffer) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        if stdin.write_all(&buffer[..n]).is_err() || stdin.flush().is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Wait for the process to complete with a reasonable timeout
        self.join(Duration::from_secs(300)); // 5 minutes timeout

        // Signal I/O tasks to finish
        process_done.store(true, Ordering::Relaxed);

        // Wait a short time for I/O tasks to complete
        let _ = tokio::time::timeout(Duration::from_secs(1), async {
            let _ = stdout_handle.await;
            let _ = stderr_handle.await;
            let _ = stdin_handle.await;
        })
        .await;

        // Return the exit code
        self.exit_code()
    }

    pub fn join(&self, duration: Duration) -> bool {
        let timeout_ms = duration.as_millis().try_into().unwrap_or(u32::MAX); // Use maximum timeout if duration is too large

        let res;
        unsafe {
            res = WaitForSingleObject(self.process_info.hProcess, timeout_ms);
        }
        match res {
            WAIT_OBJECT_0 => true,
            _ => false,
        }
    }

    /// Get the exit code of the process. Returns 1 if unable to get the exit code.
    /// This matches Unix behavior where processes that fail to execute
    /// typically return exit code 1.
    pub fn exit_code(&self) -> windows::core::Result<i32> {
        use windows::Win32::System::Threading::GetExitCodeProcess;
        let mut exit_code: u32 = 1; // Default to 1 if we fail to get the real exit code
        unsafe {
            GetExitCodeProcess(self.process_info.hProcess, &mut exit_code as *mut u32)
                .map_err(|_| windows::core::Error::from_win32())?;
        }
        Ok(exit_code as i32)
    }
}

impl Drop for WindowsProcess {
    fn drop(&mut self) {
        unsafe {
            let _ = ::windows::Win32::Foundation::CloseHandle(self.process_info.hProcess);
            let _ = ::windows::Win32::Foundation::CloseHandle(self.process_info.hThread);
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
            if ::windows::Win32::System::Threading::ResumeThread(self.process_info.hThread)
                == (-1i32 as u32)
            {
                return Err(windows::core::Error::from_win32());
            }
        }
        Ok(())
    }
}
