//! Windows process execution with mirrord layer injection.

use std::{collections::HashMap, ffi::OsString, ops::Deref, ptr};

use dll_syringe::{Syringe, process::OwnedProcess as InjectorOwnedProcess};
use mirrord_config::LayerConfig;
use str_win::string_to_u16_buffer;
use winapi::{
    shared::{
        minwindef::{BOOL, DWORD, LPVOID},
        ntdef::{HANDLE, LPCWSTR, LPWSTR},
    },
    um::{
        handleapi::CloseHandle,
        minwinbase::LPSECURITY_ATTRIBUTES,
        processenv::GetStdHandle,
        processthreadsapi::{
            LPPROCESS_INFORMATION, LPSTARTUPINFOW, PROCESS_INFORMATION, ResumeThread, STARTUPINFOW,
            TerminateProcess, CreateProcessW, GetExitCodeProcess,
        },
        synchapi::WaitForSingleObject,
        winbase::{
            CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, INFINITE, STD_ERROR_HANDLE,
            STD_OUTPUT_HANDLE, WAIT_OBJECT_0,
        },
        winnt::PHANDLE,
    },
};

use super::sync::{LayerInitEvent, MIRRORD_LAYER_INIT_EVENT_NAME};
use crate::{
    error::{LayerError, LayerResult, windows::WindowsError},
    process::windows::execution::pipes::StdioRedirection,
    proxy_connection::PROXY_CONNECTION,
    setup::layer_setup,
    socket::sockets::SHARED_SOCKETS_ENV_VAR,
};

pub mod debug;
pub mod pipes;

pub use debug::{
    format_debugger_config, get_current_process_name, is_debugger_wait_enabled,
    should_wait_for_debugger,
};

pub const MIRRORD_AGENT_ADDR_ENV: &str = "MIRRORD_AGENT_ADDR";
pub const MIRRORD_LAYER_ID_ENV: &str = "MIRRORD_LAYER_ID";
pub const MIRRORD_LAYER_FILE_ENV: &str = "MIRRORD_LAYER_FILE";

// Windows-specific child process inheritance environment variables
pub const MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID: &str = "MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID";
pub const MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID: &str = "MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID";
pub const MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR: &str = "MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR";

const LAYER_INIT_TIMEOUT_MS: u32 = 30_000;

/// Function signature for CreateProcessInternalW Windows API call.
pub type CreateProcessInternalWType = unsafe extern "system" fn(
    user_token: HANDLE,
    application_name: LPCWSTR,
    command_line: LPWSTR,
    process_attributes: LPSECURITY_ATTRIBUTES,
    thread_attributes: LPSECURITY_ATTRIBUTES,
    inherit_handles: BOOL,
    creation_flags: DWORD,
    environment: LPVOID,
    current_directory: LPCWSTR,
    startup_info: LPSTARTUPINFOW,
    process_information: LPPROCESS_INFORMATION,
    restricted_user_token: PHANDLE,
) -> BOOL;

pub struct LayerManagedProcess {
    process_info: PROCESS_INFORMATION,
    stdio_redirection: Option<StdioRedirection>,
    released: bool,
    terminate_on_drop: bool,
}

impl LayerManagedProcess {
    /// Create stdio redirection for child process output to parent
    fn create_stdio_redirection() -> LayerResult<Option<StdioRedirection>> {
        match StdioRedirection::create_for_child() {
            Ok(redirection) => Ok(Some(redirection)),
            Err(_) => Ok(None), // Fallback to no redirection
        }
    }

    /// Build Windows environment block from HashMap (UTF-16 format)
    fn build_windows_env_block(environment: &HashMap<String, String>) -> Vec<u16> {
        let mut windows_environment: Vec<u16> = Vec::new();
        for (key, value) in environment {
            let entry = format!("{}={}", key, value);
            let entry_wide = string_to_u16_buffer(&entry);
            windows_environment.extend(entry_wide);
        }
        // Unicode environment block is terminated by four zero bytes:
        // two for the last string, two more to terminate the block
        windows_environment.push(0); // Terminate the last string
        windows_environment.push(0); // Terminate the block
        windows_environment
    }

    /// Add mirrord-specific environment variables to caller's environment
    fn add_mirrord_env_vars(env_vars: &mut HashMap<String, String>, parent_event: &LayerInitEvent) {
        // Add mirrord layer initialization event
        env_vars.insert(
            MIRRORD_LAYER_INIT_EVENT_NAME.to_string(),
            parent_event.name().to_string(),
        );

        // Add other mirrord environment variables if they exist in current process
        if let Ok(agent_addr) = std::env::var(MIRRORD_AGENT_ADDR_ENV) {
            env_vars.insert(MIRRORD_AGENT_ADDR_ENV.to_string(), agent_addr);
        }
        if let Ok(layer_id) = std::env::var(MIRRORD_LAYER_ID_ENV) {
            env_vars.insert(MIRRORD_LAYER_ID_ENV.to_string(), layer_id);
        }
        if let Ok(layer_file) = std::env::var(MIRRORD_LAYER_FILE_ENV) {
            env_vars.insert(MIRRORD_LAYER_FILE_ENV.to_string(), layer_file);
        }
        
        // Forward shared sockets environment variable if it exists
        if let Ok(shared_sockets) = std::env::var(SHARED_SOCKETS_ENV_VAR) {
            env_vars.insert(SHARED_SOCKETS_ENV_VAR.to_string(), shared_sockets.clone());
            tracing::debug!("Forwarding shared sockets to child process: {}", shared_sockets);
        }
        
        // Add resolved config for child process inheritance
        // First try to get from environment variable, then fallback to encoding current config
        if let Ok(resolved_config) = std::env::var(LayerConfig::RESOLVED_CONFIG_ENV) {
            env_vars.insert(LayerConfig::RESOLVED_CONFIG_ENV.to_string(), resolved_config);
        } else {
            // Fallback: try to encode current config if layer setup is available
            // Use a safe approach that doesn't panic if setup isn't initialized
            match std::panic::catch_unwind(|| layer_setup().layer_config().encode()) {
                Ok(Ok(encoded_config)) => {
                    env_vars.insert(LayerConfig::RESOLVED_CONFIG_ENV.to_string(), encoded_config);
                    tracing::debug!("Fallback: encoded current config for child process inheritance");
                }
                Ok(Err(encode_error)) => {
                    tracing::warn!("Could not encode current config for child process: {}", encode_error);
                }
                Err(_) => {
                    tracing::error!("Layer setup not available yet, cannot provide fallback config to child process");
                }
            }
        }

        // Add Windows-specific child process inheritance variables if proxy connection exists
        if let Some(proxy_conn) = PROXY_CONNECTION.get() {
            // Pass current process ID as parent PID for child
            env_vars.insert(
                MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID.to_string(),
                std::process::id().to_string(),
            );

            // Pass current layer ID for child inheritance
            env_vars.insert(
                MIRRORD_LAYER_CHILD_PROCESS_LAYER_ID.to_string(),
                proxy_conn.layer_id().0.to_string(),
            );

            // Pass proxy address for child connection
            env_vars.insert(
                MIRRORD_LAYER_CHILD_PROCESS_PROXY_ADDR.to_string(),
                proxy_conn.proxy_addr().to_string(),
            );
        }
    }

    /// Setup stdio forwarding from child to parent handles
    fn setup_stdio_forwarding(&mut self) -> LayerResult<()> {
        if let Some(ref mut redirection) = self.stdio_redirection {
            unsafe {
                let parent_stdout = GetStdHandle(STD_OUTPUT_HANDLE);
                let parent_stderr = GetStdHandle(STD_ERROR_HANDLE);

                redirection.start_forwarding(parent_stdout, parent_stderr)?;
            }
        }
        Ok(())
    }

    /// Execute process with layer injection for CLI context, returning managed process
    pub fn execute(
        application_name: Option<String>,
        command_line: String,
        current_directory: Option<String>,
        env_vars: HashMap<String, String>,
    ) -> LayerResult<Self> {
        // For CLI context, create default parameters and use execute_with_closure
        let default_creation_flags = 0;
        let mut default_startup_info = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..unsafe { std::mem::zeroed() }
        };

        let create_process_fn = |creation_flags, environment, startup_info: &mut STARTUPINFOW| unsafe {
            // Convert strings to wide character format for Windows API
            let app_name_wide = if let Some(ref name) = application_name {
                string_to_u16_buffer(name)
            } else {
                vec![0]
            };
            let mut command_line_wide = string_to_u16_buffer(&command_line);
            let current_dir_wide = if let Some(ref dir) = current_directory {
                string_to_u16_buffer(dir)
            } else {
                vec![0]
            };

            let mut process_info: PROCESS_INFORMATION = std::mem::zeroed();

            let success = CreateProcessW(
                if application_name.is_some() {
                    app_name_wide.as_ptr()
                } else {
                    ptr::null()
                },
                command_line_wide.as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                false.into(),
                creation_flags,
                environment,
                if current_directory.is_some() {
                    current_dir_wide.as_ptr()
                } else {
                    ptr::null()
                },
                startup_info,
                &mut process_info,
            );

            if success != 0 {
                Ok(process_info)
            } else {
                Err(LayerError::WindowsProcessCreation(
                    WindowsError::last_error(),
                ))
            }
        };

        Self::execute_with_closure(
            env_vars,
            default_creation_flags,
            &mut default_startup_info,
            create_process_fn,
        )
    }

    /// Execute process with layer injection using a closure for the original function call.
    /// This method is optimized for hook contexts where all original parameters are available.
    pub fn execute_with_closure<F>(
        caller_env_vars: HashMap<String, String>,
        caller_creation_flags: DWORD,
        caller_startup_info: &mut STARTUPINFOW,
        create_process_fn: F,
    ) -> LayerResult<Self>
    where
        F: FnOnce(DWORD, LPVOID, &mut STARTUPINFOW) -> LayerResult<PROCESS_INFORMATION>,
    {
        let dll_path = std::env::var("MIRRORD_LAYER_FILE").map_err(LayerError::VarError)?;
        if !std::path::Path::new(&dll_path).exists() {
            return Err(LayerError::DllInjection(format!(
                "DLL file not found: {}",
                dll_path
            )));
        }

        let parent_event = LayerInitEvent::for_parent()?;

        // Determine the final environment: either caller's custom environment or parent environment + mirrord vars
        // Always create environment block since we need mirrord variables in both cases
        let environment = {
            let mut env = if caller_env_vars.is_empty() {
                // Caller wants to inherit parent environment - get current environment
                std::env::vars().collect()
            } else {
                // Caller provided custom environment variables
                caller_env_vars
            };
            Self::add_mirrord_env_vars(&mut env, &parent_event);
            env
        };
        let mut env_storage = Self::build_windows_env_block(&environment);
        let environment_ptr = env_storage.as_mut_ptr() as LPVOID;
        // Create stdio redirection and setup original startup info
        let stdio_redirection = Self::create_stdio_redirection()?;
        if let Some(ref redirection) = stdio_redirection {
            redirection.setup_startup_info(caller_startup_info);
        }

        // Calculate final creation flags (original + environment + suspended)
        let creation_flags = caller_creation_flags | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED;

        // Call the original function with processed parameters
        let process_info = create_process_fn(creation_flags, environment_ptr, caller_startup_info)?;

        let mut managed_process = Self {
            process_info,
            stdio_redirection,
            released: false,
            terminate_on_drop: false,
        };

        // Close child handles after process creation
        if let Some(ref mut redirection) = managed_process.stdio_redirection {
            redirection.close_child_handles();
        }

        // The process is already created and suspended by the original call
        // Now we just need to inject the DLL and resume
        managed_process.inject_and_resume(&dll_path, &parent_event)
    }

    /// Inject DLL into existing suspended process and resume execution
    fn inject_and_resume(
        mut self,
        dll_path: &str,
        parent_event: &LayerInitEvent,
    ) -> LayerResult<Self> {
        let injector_process = InjectorOwnedProcess::from_pid(self.process_info.dwProcessId)
            .map_err(|_| LayerError::ProcessNotFound(self.process_info.dwProcessId))?;

        let syringe = Syringe::for_process(injector_process);
        let payload_path = OsString::from(dll_path);

        syringe
            .inject(payload_path)
            .map_err(|e| LayerError::DllInjection(format!("Failed to inject DLL: {}", e)))?;

        match parent_event.wait_for_signal(Some(LAYER_INIT_TIMEOUT_MS))? {
            true => {}
            false => {
                return Err(LayerError::ProcessSynchronization(format!(
                    "Layer initialization timed out after {}ms for process {}",
                    LAYER_INIT_TIMEOUT_MS, self.process_info.dwProcessId
                )));
            }
        }

        // Setup stdio forwarding BEFORE resuming the thread to avoid race conditions
        self.setup_stdio_forwarding()?;

        // Resume the main thread - ResumeThread returns the previous suspend count
        // A return value of u32::MAX (0xFFFFFFFF) indicates an error
        unsafe {
            let previous_suspend_count = ResumeThread(self.process_info.hThread);
            if previous_suspend_count == u32::MAX {
                let error = WindowsError::last_error();
                return Err(LayerError::WindowsProcessCreation(error));
            }
            tracing::debug!("✅ Thread resumed, previous suspend count: {}", previous_suspend_count);
        }

        // Detach stdio forwarding to let it run independently
        if let Some(ref mut redirection) = self.stdio_redirection {
            redirection.detach_forwarding();
        }

        Ok(self)
    }

    /// Release process from management (won't be terminated on drop)
    pub fn release(mut self) -> PROCESS_INFORMATION {
        self.released = true;
        self.process_info
    }

    /// Wait for process to exit and return exit code
    pub fn wait_until_exit(self) -> LayerResult<u32> {
        let exit_code = unsafe {
            let wait_result = WaitForSingleObject(self.process_info.hProcess, INFINITE);
            if wait_result != WAIT_OBJECT_0 {
                return Err(LayerError::WindowsProcessCreation(
                    WindowsError::last_error(),
                ));
            }

            let mut exit_code = 0u32;
            if GetExitCodeProcess(
                self.process_info.hProcess,
                &mut exit_code,
            ) == 0
            {
                return Err(LayerError::WindowsProcessCreation(
                    WindowsError::last_error(),
                ));
            }
            exit_code
        };

        Ok(exit_code)
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
        if let Some(ref mut redirection) = self.stdio_redirection {
            redirection.stop_forwarding();
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
