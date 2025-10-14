use std::{
    collections::HashMap,
    error::Error,
    ffi::OsStr,
    fs::File,
    process::{Command, Stdio},
    ptr,
};

use ::windows::{
    Win32::{
        Foundation::{
            CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, INVALID_HANDLE_VALUE,
            SetHandleInformation,
        },
        Security::SECURITY_ATTRIBUTES,
        System::{
            Pipes as Win32Pipes,
            Threading::{self as Win32Threading, STARTF_USESTDHANDLES},
        },
    },
    core::{self as windows_core, PCWSTR, PWSTR},
};
use mirrord_layer_lib::process::environment::EnvMap;
use str_win::string_to_u16_buffer;

use crate::{
    error::{ProcessExecError, ProcessExecResult},
    execution::windows::{
        injection::WindowsProcessSuspendedExtInject,
        process::{HandleWrapper, WindowsProcess, WindowsProcessExtSuspended},
    },
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

    pub fn envs<K, V>(mut self, envs: HashMap<K, V>) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        let mut envs: HashMap<String, String> = envs
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();

        // add WSLENV=KEY1:KEY2:... env-var for WSL environment variable propagation support
        // this is needed when executing bash which is not aware of the envvars without it.
        // see https://devblogs.microsoft.com/commandline/share-environment-vars-between-wsl-and-windows/
        if !envs.contains_key("WSLENV") {
            let wsl_env_value = envs.keys().cloned().collect::<Vec<_>>().join(":");
            envs.insert("WSLENV".to_string(), wsl_env_value);
        }

        self.envs = EnvMap::from(envs);
        self
    }

    fn new_annonymous_pipe() -> Result<(HANDLE, HANDLE), windows_core::Error> {
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
        }
        Ok((read_pipe_handle, write_pipe_handle))
    }

    fn new_pipes_for_stdio(
        stdio: Option<Stdio>,
        should_inherit_read: bool,
        should_inherit_write: bool,
    ) -> ProcessExecResult<(HANDLE, HANDLE)> {
        match stdio {
            Some(_) => match Self::new_annonymous_pipe() {
                Ok((read_pipe_handle, write_pipe_handle)) => unsafe {
                    // set handle inheritance according to arguments
                    if !should_inherit_read {
                        SetHandleInformation(
                            read_pipe_handle,
                            HANDLE_FLAG_INHERIT.0,
                            HANDLE_FLAGS(0),
                        )?;
                    }
                    if !should_inherit_write {
                        SetHandleInformation(
                            write_pipe_handle,
                            HANDLE_FLAG_INHERIT.0,
                            HANDLE_FLAGS(0),
                        )?;
                    }
                    Ok((read_pipe_handle, write_pipe_handle))
                },
                Err(e) => Err(ProcessExecError::PipeError(e)),
            },
            // stdio not specified - dont fail, just return invalid handles silently
            None => Ok((INVALID_HANDLE_VALUE, INVALID_HANDLE_VALUE)),
        }
    }

    pub fn inject_and_spawn(self, dll_path: String) -> Result<WindowsProcess, Box<dyn Error>> {
        let child = self.spawn_suspend()?;
        child.inject_dll(dll_path)?;
        child.resume()?;
        Ok(child)
    }

    pub fn spawn_suspend(self) -> ProcessExecResult<WindowsProcess>
    where
        WindowsProcess: WindowsProcessExtSuspended,
    {
        let (stdin_pipe_rd, stdin_pipe_wr) = Self::new_pipes_for_stdio(self.stdin, true, false)?;
        let (stdout_pipe_rd, stdout_pipe_wr) = Self::new_pipes_for_stdio(self.stdout, false, true)?;
        let (stderr_pipe_rd, stderr_pipe_wr) = Self::new_pipes_for_stdio(self.stderr, false, true)?;

        let startup_info = Win32Threading::STARTUPINFOW {
            cb: std::mem::size_of::<Win32Threading::STARTUPINFOW>() as u32,
            hStdInput: stdin_pipe_rd,
            hStdOutput: stdout_pipe_wr,
            hStdError: stdout_pipe_wr,
            dwFlags: STARTF_USESTDHANDLES,
            ..Default::default()
        };

        let program: Vec<u16> =
            string_to_u16_buffer(self.command.get_program().to_string_lossy().as_ref());
        let args: String = self
            .command
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<String>>()
            .join(" ");

        let mut wide_cmdline: Vec<u16> = string_to_u16_buffer(&args);

        // Always use explicit environment block to match Unix execve behavior
        // The environment in self.envs is already complete (prepared by main.rs)
        let environment_block = self.envs.to_windows_env_block();

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
                PCWSTR(program.as_ptr()),
                Some(PWSTR(wide_cmdline.as_mut_ptr())),
                Some(ptr::null()),
                Some(ptr::null()),
                true,
                creation_flags,
                Some(environment_block.as_ptr() as *const _),
                PCWSTR::null(),
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

        Ok(WindowsProcess {
            process_info,
            stdin: child_stdin,
            stdout: child_stdout,
            stderr: child_stderr,
        })
    }
}
