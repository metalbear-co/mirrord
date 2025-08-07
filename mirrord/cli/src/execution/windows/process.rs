use std::fs::File;
use std::io::Read;
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::time::Duration;
use windows::Win32::{
    Foundation::{HANDLE, WAIT_OBJECT_0,}, 
    System::Threading::{self as Win32Threading, WaitForSingleObject}
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
/// Wraps a Windows process started in suspended mode
pub struct SuspendedProcess {
    pub process_info: Win32Threading::PROCESS_INFORMATION,
    pub stdin: File,
    pub stdout: File,
    pub stderr: File,
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

    pub fn join(&self, duration: Duration) -> bool {
        let res;
        unsafe {
            res = WaitForSingleObject(
                self.process_info.hProcess,
                duration
                    .as_millis()
                    .try_into()
                    .expect("duration must fit u32"),
            );
        }
        match res {
            WAIT_OBJECT_0 => true,
            _ => false,
        }
    }

    pub fn read_stdout(&mut self) -> String{
        let mut contents = String::new();
        self.stdout.read_to_string(&mut contents).expect("failed to read stdout to String");
        contents
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
