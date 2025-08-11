mod common;
pub mod injector;

use std::{
    error::Error,
    fs::File,
    io::{self, Read},
    mem,
    os::windows::io::{FromRawHandle, OwnedHandle},
    ptr,
    time::Duration,
};

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation,
            WAIT_OBJECT_0,
        },
        Security::SECURITY_ATTRIBUTES,
        System::{
            Pipes as Win32Pipes,
            Threading::{self as Win32Threading, STARTF_USESTDHANDLES, WaitForSingleObject},
        },
    },
    core::{self as windows_core, PCWSTR, PWSTR},
};

use crate::commandline::TargetCommandline;
pub use crate::process::common::TargetProcess;

impl TargetProcess {
    pub fn execute(
        target_commandline: TargetCommandline,
        suspended: bool,
    ) -> Result<TargetProcess, Box<dyn Error>> {
        let (appname, mut cmdline) = target_commandline.to_wstr_tup();

        let creation_flags = {
            let mut cf = Win32Threading::PROCESS_CREATION_FLAGS::default();
            if suspended {
                cf |= Win32Threading::CREATE_SUSPENDED;
            };
            cf
        };
        let mut process_information = Win32Threading::PROCESS_INFORMATION::default();

        let (stdout_pipe_rd, stdout_pipe_wr) =
            Self::new_annonymous_pipe().expect("failed to open pipe for child process stdout");
        let startup_info = Win32Threading::STARTUPINFOW {
            cb: std::mem::size_of::<Win32Threading::STARTUPINFOW>() as u32,
            hStdOutput: stdout_pipe_wr, // write stdout to our write_pipe
            dwFlags: STARTF_USESTDHANDLES, // use std___ handles
            ..Default::default()
        };

        unsafe {
            Win32Threading::CreateProcessW(
                Into::<PCWSTR>::into(&appname),
                Some(Into::<PWSTR>::into(&mut cmdline)),
                None,
                None,
                true,
                creation_flags,
                None,
                None,
                &startup_info,
                &mut process_information,
            )
            .expect("Failed to CreateProcess");

            // must close write side of the pipe (see: https://stackoverflow.com/q/54416116)
            CloseHandle(stdout_pipe_wr).expect("failed to close write pipe handle");
        };

        let stdout = unsafe { File::from(OwnedHandle::from_raw_handle(stdout_pipe_rd.0)) };

        Ok(TargetProcess {
            process_information,
            stdout,
        })
    }

    pub fn resume(&self) -> windows_core::Result<()> {
        unsafe {
            // ResumeThread: If the function fails, the return value is (DWORD) -1
            if Win32Threading::ResumeThread(self.process_information.hThread) == (-1i32 as u32) {
                return Err(windows_core::Error::from_win32());
            }
        }
        Ok(())
    }

    pub fn join(&self, duration: Duration) -> bool {
        let res;
        unsafe {
            res = WaitForSingleObject(
                self.process_information.hProcess,
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

    fn new_annonymous_pipe() -> Result<(HANDLE, HANDLE), windows_core::Error> {
        let mut read_pipe_handle = HANDLE::default();
        let mut write_pipe_handle = HANDLE::default();

        let sa = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            bInheritHandle: true.into(),
            lpSecurityDescriptor: ptr::null_mut(),
        };
        let sa_ptr = std::ptr::from_ref(&sa);
        unsafe {
            // create ours <-> theirs pipe
            Win32Pipes::CreatePipe(
                &mut read_pipe_handle,
                &mut write_pipe_handle,
                Some(sa_ptr),
                0,
            )?;

            // Ensure the read handle to the pipe for STDOUT is not inherited.
            SetHandleInformation(read_pipe_handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))?;
        }
        Ok((read_pipe_handle, write_pipe_handle))
    }

    pub fn output(&mut self) -> Result<String, io::Error> {
        let mut s = String::new();
        self.stdout.read_to_string(&mut s)?;
        Ok(s)
    }
}
