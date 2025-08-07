use std::collections::HashMap;
use std::error::Error;
use std::ffi::OsStr;
use std::fs::File;
use std::process::{Command, Stdio};
use std::ptr;
use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use std::time::Duration;
use ::windows::{
    core::{self as windows_core, PCWSTR, PWSTR},
    Win32::{
        Foundation::{
            CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT,
            INVALID_HANDLE_VALUE,
        },
        Security::SECURITY_ATTRIBUTES,
        System::{
            Pipes as Win32Pipes,
            Threading::{self as Win32Threading, STARTF_USESTDHANDLES},
        },
    },
};
use crate::execution::windows::{
    env_vars::EnvMap,
    injection::SuspendedProcessExtInject,
    process::{HandleWrapper, SuspendedProcess}, 
};


#[derive(Debug)]
pub struct WindowsCommand {
    command: Command,
    stdin: Option<Stdio>,
    stdout: Option<Stdio>,
    stderr: Option<Stdio>,
    envs: EnvMap,
}

impl WindowsCommand {
    pub fn new<S: AsRef<OsStr>>(program: S) -> Self {
        Self {
            command: Command::new(program),
            stdin: None,
            stdout: None,
            stderr: None,
            envs: EnvMap::default(),
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.args(args);
        self
    }

    pub fn stdin(mut self, cfg: Stdio) -> Self {
        self.stdin = Some(cfg);
        self
    }

    pub fn stdout(mut self, cfg: Stdio) -> Self {
        self.stdout = Some(cfg);
        self
    }

    pub fn stderr(mut self, cfg: Stdio) -> Self {
        self.stderr = Some(cfg);
        self
    }

    pub fn envs<K: Into<String>, V: Into<String>>(mut self, envs: HashMap<K, V>) -> Self {
        self.envs = EnvMap::from(envs);
        self
    }

    fn new_annonymous_pipe(should_inherit_read: bool, should_inherit_write: bool) -> Result<(HANDLE, HANDLE), windows_core::Error> {
        let mut read_pipe_handle = HANDLE::default();
        let mut write_pipe_handle = HANDLE::default();

        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
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

            
            if !should_inherit_read {
                SetHandleInformation(read_pipe_handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))?;
            }

            if !should_inherit_write {
                SetHandleInformation(write_pipe_handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))?;
            }
        }
        Ok((read_pipe_handle, write_pipe_handle))
    }
    
    pub fn inject_and_spawn(self, dll_path: String) -> Result<(), Box<dyn Error>> {
        let mut child = self.spawn_suspend()?;
        println!("Spawned process in suspended state");
        
        child.inject_dll(dll_path)?;
        println!("injected dll");

        child.resume()?;
        println!("resumed");

        if child.join(Duration::from_secs(30)) {
            println!("child is dead ;(");
        } else {
            println!("child did not die ;(");
        }
        println!("Child Stdout: {}", child.read_stdout());
        
        Ok(())
    }

    pub fn spawn_suspend(self) -> std::io::Result<SuspendedProcess> {

        let (stdin_pipe_rd, stdin_pipe_wr) = {
            if let Some(_) = self.stdin {
                // Ensure the write handle to the pipe for STDIN is not inherited.
                Self::new_annonymous_pipe(true, false).expect("failed to open pipe for child process stdin")
            } else {
                (INVALID_HANDLE_VALUE, INVALID_HANDLE_VALUE)
            }
        };
        
        let (stdout_pipe_rd, stdout_pipe_wr) = {
            if let Some(_) = self.stdout {
                // Ensure the read handle to the pipe for STDOUT is not inherited.
                Self::new_annonymous_pipe(false, true).expect("failed to open pipe for child process stdout")
            } else {
                (INVALID_HANDLE_VALUE, INVALID_HANDLE_VALUE)
            }
        };

        let (stderr_pipe_rd, stderr_pipe_wr) = {
            if let Some(_) = self.stderr {
                // Ensure the read handle to the pipe for STDERR is not inherited.
                Self::new_annonymous_pipe(false, true).expect("failed to open pipe for child process stderr")
            } else {
                (INVALID_HANDLE_VALUE, INVALID_HANDLE_VALUE)
            }
        };

        let startup_info = Win32Threading::STARTUPINFOW {
            cb: std::mem::size_of::<Win32Threading::STARTUPINFOW>() as u32,
            hStdInput: stdin_pipe_rd,
            hStdOutput: stdout_pipe_wr,
            hStdError: stdout_pipe_wr,
            dwFlags: STARTF_USESTDHANDLES, // use std___ handles
            ..Default::default()
        };

        let program: Vec<u16> = OsStr::new(self.command.get_program()).encode_wide().chain(once(0)).collect();
        let args: String = self.command
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<String>>().join(" ");

        let mut wide_cmdline: Vec<u16> = OsStr::new(&args)
            .encode_wide()
            .chain(once(0))
            .collect();

        // todo!(take care of env_clear and self.envs empty situation)
        let environment_block = self.envs.to_windows_env_block();
        let lp_environment = environment_block.as_ptr() as *const _;

        let creation_flags = {
            let mut cf = Win32Threading::PROCESS_CREATION_FLAGS::default();
            cf |= Win32Threading::CREATE_UNICODE_ENVIRONMENT;
            cf |= Win32Threading::CREATE_SUSPENDED;
            cf
        };
        let mut process_info = Win32Threading::PROCESS_INFORMATION::default();

        let (child_stdin, child_stdout, child_stderr);
        unsafe {
            Win32Threading::CreateProcessW(
                PCWSTR(program.as_ptr()),                    // ApplicationName
                Some(PWSTR(wide_cmdline.as_mut_ptr())),  // CommandLine
                Some(ptr::null()),                    // ProcessAttributes
                Some(ptr::null()),                    // ThreadAttributes
                true,                           // InheritHandles
                creation_flags,
                Some(lp_environment),
                PCWSTR::null(),                 // CurrentDirectory
                &startup_info,
                &mut process_info,
            )?;

            child_stdin = File::from(HandleWrapper(stdin_pipe_wr));
            // must close read side of the pipe (see: https://stackoverflow.com/q/54416116)
            CloseHandle(stdin_pipe_rd).expect("failed to close write pipe handle");
            
            child_stdout = File::from(HandleWrapper(stdout_pipe_rd));
            // must close write side of the pipe (see: https://stackoverflow.com/q/54416116)
            CloseHandle(stdout_pipe_wr).expect("failed to close write pipe handle");

            child_stderr = File::from(HandleWrapper(stderr_pipe_rd));
            // must close write side of the pipe (see: https://stackoverflow.com/q/54416116)
            CloseHandle(stderr_pipe_wr).expect("failed to close write pipe handle");
        };

        Ok(SuspendedProcess {
            process_info: process_info,
            stdin: child_stdin,
            stdout: child_stdout,
            stderr: child_stderr,
        })
    }

}