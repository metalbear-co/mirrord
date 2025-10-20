//! Windows process execution with mirrord layer injection.

use std::{collections::HashMap, ffi::OsString, ops::Deref, ptr};

use dll_syringe::{Syringe, process::OwnedProcess as InjectorOwnedProcess};
use str_win::string_to_u16_buffer;
use winapi::{
    shared::{
        minwindef::{LPVOID},
        ntdef::HANDLE,
    },
    um::{
        handleapi::CloseHandle,
        minwinbase::LPSECURITY_ATTRIBUTES,
        processenv::GetStdHandle,
        processthreadsapi::{
            CreateProcessW, LPPROCESS_INFORMATION, PROCESS_INFORMATION, ResumeThread,
            TerminateProcess, STARTUPINFOW,
        },
        winbase::{
            CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        },
        winnt::PHANDLE,
    },
};

use super::sync::{LayerInitEvent, MIRRORD_LAYER_INIT_EVENT_NAME};
use crate::{
    error::{LayerError, LayerResult, windows::WindowsError},
    process::windows::execution::pipes::{
        StdioRedirection,
    },
};

pub mod pipes;
pub mod debug;

pub use debug::{
    format_debugger_config, get_current_process_name, is_debugger_wait_enabled,
    should_wait_for_debugger,
};

pub const MIRRORD_AGENT_ADDR_ENV: &str = "MIRRORD_AGENT_ADDR";
pub const MIRRORD_LAYER_ID_ENV: &str = "MIRRORD_LAYER_ID";
pub const MIRRORD_LAYER_FILE_ENV: &str = "MIRRORD_LAYER_FILE";

const LAYER_INIT_TIMEOUT_MS: u32 = 30_000;

/// Function signature for CreateProcessInternalW Windows API call.
pub type CreateProcessInternalWType = unsafe extern "system" fn(
    user_token: HANDLE,
    application_name: winapi::shared::ntdef::LPCWSTR,
    command_line: winapi::shared::ntdef::LPWSTR,
    process_attributes: LPSECURITY_ATTRIBUTES,
    thread_attributes: LPSECURITY_ATTRIBUTES,
    inherit_handles: winapi::shared::minwindef::BOOL,
    creation_flags: winapi::shared::minwindef::DWORD,
    environment: LPVOID,
    current_directory: winapi::shared::ntdef::LPCWSTR,
    startup_info: winapi::um::processthreadsapi::LPSTARTUPINFOW,
    process_information: LPPROCESS_INFORMATION,
    restricted_user_token: PHANDLE,
) -> winapi::shared::minwindef::BOOL;

pub struct LayerManagedProcess {
    process_info: PROCESS_INFORMATION,
    stdio_redirection: Option<StdioRedirection>,
    released: bool,
    terminate_on_drop: bool,
}

pub struct ProcessResult {
    pub info: PROCESS_INFORMATION,
    pub exit_code: Option<u32>,
}

impl Deref for ProcessResult {
    type Target = PROCESS_INFORMATION;

    fn deref(&self) -> &Self::Target {
        &self.info
    }
}

impl LayerManagedProcess {
    /// Create stdio redirection for redirecting child process output to parent.
    fn create_stdio_redirection() -> LayerResult<Option<StdioRedirection>> {
        match StdioRedirection::create_for_child() {
            Ok(redirection) => Ok(Some(redirection)),
            Err(_) => Ok(None), // Fallback to no redirection
        }
    }

    /// Setup pipe forwarding to redirect child stdio to parent stdio.
    fn setup_stdio_forwarding(&mut self) -> LayerResult<()> {
        if let Some(ref mut redirection) = self.stdio_redirection {
            unsafe {
                let parent_stdout = GetStdHandle(STD_OUTPUT_HANDLE);
                let parent_stderr = GetStdHandle(STD_ERROR_HANDLE);
                
                redirection.start_forwarding(parent_stdout, parent_stderr)?;
                redirection.close_child_handles();
            }
        }
        Ok(())
    }

    fn create_suspended(
        application_name: &Option<String>,
        command_line: &str,
        current_directory: &Option<String>,
        environment: &HashMap<String, String>,
        original_fn: Option<&'static CreateProcessInternalWType>,
        stdio_redirection: Option<StdioRedirection>,
    ) -> LayerResult<Self> {
        unsafe {
            let creation_flags = CREATE_SUSPENDED;
            
            // Convert environment variables to Windows format for process creation
            let mut actual_creation_flags = creation_flags;
            let mut windows_environment: Vec<u16> = Vec::new();
            let environment_ptr = if !environment.is_empty() {
                actual_creation_flags |= CREATE_UNICODE_ENVIRONMENT;

                for (key, value) in environment {
                    let env_entry = format!("{}={}", key, value);
                    let entry_wide = string_to_u16_buffer(&env_entry);
                    windows_environment.extend(entry_wide);
                }
                windows_environment.push(0);
                windows_environment.as_mut_ptr() as LPVOID
            } else {
                ptr::null_mut()
            };

            let (_app_name_wide, app_name_ptr) = if let Some(name) = application_name {
                let wide = string_to_u16_buffer(name);
                let ptr = wide.as_ptr();
                (Some(wide), ptr)
            } else {
                (None, std::ptr::null())
            };

            let mut command_line_wide = string_to_u16_buffer(command_line);

            let (_current_dir_wide, current_dir_ptr) = if let Some(dir) = current_directory {
                let wide = string_to_u16_buffer(dir);
                let ptr = wide.as_ptr();
                (Some(wide), ptr)
            } else {
                (None, std::ptr::null())
            };

            let mut startup_info = STARTUPINFOW {
                cb: std::mem::size_of::<STARTUPINFOW>() as u32,
                ..std::mem::zeroed()
            };

            if let Some(ref redirection) = stdio_redirection {
                redirection.setup_startup_info(&mut startup_info);
            }

            let mut process_info: PROCESS_INFORMATION = std::mem::zeroed();

            let success = if let Some(original) = original_fn {
                // call original CreateProcessInternalW function
                original(
                    ptr::null_mut(),
                    app_name_ptr,
                    command_line_wide.as_mut_ptr(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    false.into(),
                    actual_creation_flags,
                    environment_ptr,
                    current_dir_ptr,
                    &mut startup_info,
                    &mut process_info,
                    ptr::null_mut(),
                ) != 0
            } else {
                CreateProcessW(
                    app_name_ptr,
                    command_line_wide.as_mut_ptr(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    false.into(),
                    actual_creation_flags,
                    environment_ptr,
                    current_dir_ptr,
                    &mut startup_info,
                    &mut process_info,
                ) != 0
            };

            if !success {
                let error = WindowsError::last_error();
                return Err(LayerError::WindowsProcessCreation(error));
            }

            Ok(Self {
                process_info,
                stdio_redirection,
                released: false,
                terminate_on_drop: true,
            })
        }
    }

    /// Implementation for execute methods that handles layer injection
    fn execute_with_layer(
        application_name: Option<String>,
        command_line: String,
        current_directory: Option<String>,
        env_vars: HashMap<String, String>,
        original_fn: Option<&'static CreateProcessInternalWType>,
        enable_stdio_redirection: bool,
    ) -> LayerResult<ProcessResult> {
        // Prepare child environment with DLL path validation and mirrord variables
        let dll_path = std::env::var("MIRRORD_LAYER_FILE").map_err(LayerError::VarError)?;
        if !std::path::Path::new(&dll_path).exists() {
            return Err(LayerError::DllInjection(format!("DLL file not found: {}", dll_path)));
        }

        let parent_event = LayerInitEvent::for_parent()?;
        
        let mut child_environment = std::env::vars().collect::<HashMap<String, String>>();
        child_environment.extend(env_vars);
        child_environment.insert(MIRRORD_LAYER_INIT_EVENT_NAME.to_string(), parent_event.name().to_string());

        // Create stdio redirection if needed
        let stdio_redirection = if enable_stdio_redirection {
            Self::create_stdio_redirection()?
        } else {
            None
        };

        let process = Self::create_suspended(
            &application_name,
            &command_line,
            &current_directory,
            &child_environment,
            original_fn,
            stdio_redirection,
        )?;

        let managed_process = process
            .setup_with_layer(&dll_path, &parent_event)?;

        let process_info = *managed_process;

        Ok(ProcessResult {
            info: process_info,
            exit_code: None,
        })
    }

    /// Execute process with mirrord layer injection
    pub fn execute_from_cli(
        application_name: Option<String>,
        command_line: String,
        current_directory: Option<String>,
        env_vars: HashMap<String, String>,
    ) -> LayerResult<ProcessResult> {
        Self::execute_with_layer(
            application_name,
            command_line,
            current_directory,
            env_vars,
            None,
            false,
        )
    }

    /// Execute process from within a hook context using the original function pointer.
    /// This is specifically designed for Windows API hooks that intercept process creation
    /// and need to call the original CreateProcessInternalW function while enabling stdio redirection.
    pub fn execute_from_hook(
        application_name: Option<String>,
        command_line: String,
        current_directory: Option<String>,
        env_vars: HashMap<String, String>,
        original_fn: &'static CreateProcessInternalWType,
    ) -> LayerResult<ProcessResult> {
        Self::execute_with_layer(
            application_name,
            command_line,
            current_directory,
            env_vars,
            Some(original_fn),
            true,
        )
    }

    fn inject_layer(self, dll_path: &str) -> LayerResult<Self> {
        let injector_process = InjectorOwnedProcess::from_pid(self.process_info.dwProcessId)
            .map_err(|_| LayerError::ProcessNotFound(self.process_info.dwProcessId))?;

        let syringe = Syringe::for_process(injector_process);
        let payload_path = OsString::from(dll_path);

        syringe
            .inject(payload_path)
            .map_err(|e| LayerError::DllInjection(format!("Failed to inject DLL: {}", e)))?;

        Ok(self)
    }

    fn wait_for_initialization(self, parent_event: &LayerInitEvent) -> LayerResult<Self> {
        match parent_event.wait_for_signal(Some(LAYER_INIT_TIMEOUT_MS))? {
            true => Ok(self),
            false => Err(LayerError::ProcessSynchronization(format!(
                "Layer initialization timed out after {}ms for process {}",
                LAYER_INIT_TIMEOUT_MS, self.process_info.dwProcessId
            ))),
        }
    }

    fn resume(self) -> LayerResult<Self> {
        unsafe {
            if ResumeThread(self.process_info.hThread) == u32::MAX {
                let error = WindowsError::last_error();
                return Err(LayerError::WindowsProcessCreation(error));
            }
        }

        Ok(self)
    }

    fn setup_with_layer(
        self,
        dll_path: &str,
        parent_event: &LayerInitEvent,
    ) -> LayerResult<Self> {
        self.inject_layer(dll_path)?
            .wait_for_initialization(parent_event)?
            .resume()?
            .start_stdio_forwarding()
    }

    /// Start stdio forwarding if pipes are configured.
    fn start_stdio_forwarding(mut self) -> LayerResult<Self> {
        self.setup_stdio_forwarding()?;
        Ok(self)
    }
}

impl Deref for LayerManagedProcess {
    type Target = PROCESS_INFORMATION;

    fn deref(&self) -> &Self::Target {
        &self.process_info
    }
}

impl Drop for LayerManagedProcess {
    fn drop(&mut self) {
        // Stop stdio forwarding first
        if let Some(ref mut redirection) = self.stdio_redirection {
            let _ = redirection.stop_forwarding(); // Best effort
        }

        if !self.released {
            if self.terminate_on_drop {
                unsafe {
                    TerminateProcess(self.process_info.hProcess, 1);
                }
            }
            unsafe {
                CloseHandle(self.process_info.hProcess);
                CloseHandle(self.process_info.hThread);
            }
        }
    }
}
