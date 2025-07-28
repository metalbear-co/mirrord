use std::fs::File;
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::process::Stdio;
use windows::Win32::{Foundation::HANDLE, System::Threading::{self as Win32Threading}};

pub struct HandleWrapper(pub HANDLE);

impl From<HandleWrapper> for Stdio {
    fn from(handle_wrapper: HandleWrapper) -> Self {
        unsafe {
            // SAFETY: Caller guarantees the HANDLE is valid and suitable for use as a stdio stream.
            let raw_handle = handle_wrapper.0;
            let file = File::from_raw_handle(raw_handle.0 as RawHandle);
            Stdio::from(file)
        }
    }
}
/// Wraps a Windows process started in suspended mode
pub struct SuspendedProcess {
    pub process_info: Win32Threading::PROCESS_INFORMATION,
    pub stdin: Option<Stdio>,
    pub stdout: Option<Stdio>,
    pub stderr: Option<Stdio>,
}

impl SuspendedProcess {
    pub fn resume(&self) -> windows::core::Result<()> {
        unsafe {
            // ResumeThread: If the function fails, the return value is (DWORD) -1
            if ::windows::Win32::System::Threading::ResumeThread(self.process_info.hThread) == (-1i32 as u32) {
                return Err(windows::core::Error::from_win32());
            }
        }
        Ok(())
    }
}

impl Drop for SuspendedProcess {
    fn drop(&mut self) {
        unsafe {
            let _ = ::windows::Win32::Foundation::CloseHandle(self.process_info.hProcess);
            let _ = ::windows::Win32::Foundation::CloseHandle(self.process_info.hThread);
        }
    }
}
