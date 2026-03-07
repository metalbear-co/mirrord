//! Windows process execution with mirrord layer injection.

use std::{collections::HashMap, ffi::OsString, ops::Deref, ptr};

use base64::prelude::*;
use dll_syringe::{Syringe, process::OwnedProcess as InjectorOwnedProcess};
use mirrord_config::LayerConfig;
use str_win::string_to_u16_buffer;
use winapi::{
    shared::{
        minwindef::{BOOL, DWORD, LPVOID},
        ntdef::{HANDLE, LPCWSTR, LPWSTR},
    },
    um::{
        handleapi::{CloseHandle, SetHandleInformation},
        minwinbase::LPSECURITY_ATTRIBUTES,
        processenv::GetStdHandle,
        processthreadsapi::{
            CreateProcessW, GetExitCodeProcess, LPPROCESS_INFORMATION, LPSTARTUPINFOW,
            PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, TerminateProcess,
        },
        synchapi::WaitForSingleObject,
        winbase::{
            CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, HANDLE_FLAG_INHERIT, INFINITE,
            STARTF_USESTDHANDLES, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            WAIT_OBJECT_0,
        },
        winnt::PHANDLE,
    },
};

use super::sync::{LayerInitEvent, MIRRORD_LAYER_INIT_EVENT_NAME};
use crate::{
    error::{LayerError, LayerResult, windows::WindowsError},
    logging::MIRRORD_LAYER_LOG_PATH,
    proxy_connection::PROXY_CONNECTION,
    setup::setup,
    socket::{SOCKETS, sockets::SHARED_SOCKETS_ENV_VAR},
};

pub mod debug;

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

/// Environment variables that should be explicitly forwarded from parent to child process
const FORWARDED_ENV_VARS: &[&str] = &[
    MIRRORD_AGENT_ADDR_ENV,
    MIRRORD_LAYER_ID_ENV,
    MIRRORD_LAYER_FILE_ENV,
    MIRRORD_LAYER_LOG_PATH,
    "RUST_LOG",
    "RUST_BACKTRACE",
];

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
    released: bool,
    terminate_on_drop: bool,
}

impl LayerManagedProcess {
    /// Ensure a handle is inheritable for child processes
    fn ensure_handle_inheritable(handle: HANDLE) -> LayerResult<HANDLE> {
        if handle.is_null() {
            return Ok(handle); // Invalid handles can't be made inheritable
        }

        unsafe {
            if SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0 {
                // If SetHandleInformation fails, log a warning but continue
                // Some handles (like console handles) might already be inheritable
                tracing::warn!(
                    "Failed to set handle inheritance: {}",
                    WindowsError::last_error()
                );
            }
        }
        Ok(handle)
    }

    /// Build Windows environment block from HashMap (UTF-16 format)
    fn build_windows_env_block(environment: &HashMap<String, String>) -> Vec<u16> {
        let mut windows_environment: Vec<u16> = Vec::new();
        for (key, value) in environment {
            let entry = format!("{}={}", key, value);
            let entry_wide = string_to_u16_buffer(&entry);
            windows_environment.extend(entry_wide);
        }
        // add environment block null terminator
        windows_environment.push(0);
        windows_environment
    }

    /// Add mirrord-specific environment variables to caller's environment
    fn add_mirrord_env_vars(env_vars: &mut HashMap<String, String>, parent_event: &LayerInitEvent) {
        // Add mirrord layer initialization event
        env_vars.insert(
            MIRRORD_LAYER_INIT_EVENT_NAME.to_string(),
            parent_event.name().to_string(),
        );

        // Forward explicitly configured environment variables from parent to child
        for &env_var in FORWARDED_ENV_VARS {
            if let Ok(value) = std::env::var(env_var) {
                env_vars.insert(env_var.to_string(), value.clone());
            } else {
                tracing::debug!("No {} found in current process environment", env_var);
            }
        }

        // Encode and forward current socket state to child process (like Unix prepare_execve_envp)
        let encoded_sockets = match SOCKETS.lock() {
            Ok(lock) => {
                let shared_sockets = lock
                    .iter()
                    .map(|(key, value)| (*key, value))
                    .collect::<Vec<_>>();
                let socket_count = shared_sockets.len();
                let encoded = bincode::encode_to_vec(shared_sockets, bincode::config::standard())
                    .map(|bytes| BASE64_URL_SAFE.encode(bytes));
                drop(lock);

                match encoded {
                    Ok(encoded_sockets) => Some((encoded_sockets, socket_count)),
                    Err(error) => {
                        tracing::warn!("Failed to encode shared sockets: {}", error);
                        None
                    }
                }
            }
            Err(error) => {
                tracing::warn!("Failed to lock shared sockets: {}", error);
                None
            }
        };

        if let Some((encoded_sockets, socket_count)) = encoded_sockets {
            env_vars.insert(SHARED_SOCKETS_ENV_VAR.to_string(), encoded_sockets.clone());
            tracing::debug!(
                "Encoded and forwarding {} shared sockets to child process: {}",
                socket_count,
                encoded_sockets
            );
        } else if let Ok(existing_sockets) = std::env::var(SHARED_SOCKETS_ENV_VAR) {
            env_vars.insert(SHARED_SOCKETS_ENV_VAR.to_string(), existing_sockets.clone());
            tracing::debug!(
                "Fallback: forwarding existing shared sockets: {}",
                existing_sockets
            );
        }

        // Add resolved config for child process inheritance
        // Only add if not already present in the environment we're building
        if !env_vars.contains_key(LayerConfig::RESOLVED_CONFIG_ENV) {
            // First try to get from current environment variable, then fallback to encoding current
            // config
            if let Ok(resolved_config) = std::env::var(LayerConfig::RESOLVED_CONFIG_ENV) {
                env_vars.insert(
                    LayerConfig::RESOLVED_CONFIG_ENV.to_string(),
                    resolved_config,
                );
            } else {
                // Fallback: try to encode current config if layer setup is available
                // Use a safe approach that doesn't panic if setup isn't initialized
                match std::panic::catch_unwind(|| setup().layer_config().encode()) {
                    Ok(Ok(encoded_config)) => {
                        env_vars
                            .insert(LayerConfig::RESOLVED_CONFIG_ENV.to_string(), encoded_config);
                        tracing::debug!(
                            "Fallback: encoded current config for child process inheritance"
                        );
                    }
                    Ok(Err(encode_error)) => {
                        tracing::warn!(
                            "Could not encode current config for child process: {}",
                            encode_error
                        );
                    }
                    Err(_) => {
                        tracing::error!(
                            "Layer setup not available yet, cannot provide fallback config to child process"
                        );
                    }
                }
            }
        }

        // Add Windows-specific child process inheritance variables if proxy connection exists
        #[allow(static_mut_refs)]
        unsafe {
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
    }

    /// Execute process with layer injection for CLI context, returning managed process
    pub fn execute<P>(
        application_name: Option<String>,
        command_line: String,
        current_directory: Option<String>,
        env_vars: HashMap<String, String>,
        progress: Option<P>,
    ) -> LayerResult<Self>
    where
        P: mirrord_progress::Progress,
    {
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
                // Enable handle inheritance so child can inherit console handles
                true.into(),
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
            progress,
        )
    }

    /// Execute process with layer injection using a closure for the original function call.
    /// This method is optimized for hook contexts where all original parameters are available.
    pub fn execute_with_closure<F, P>(
        caller_env_vars: HashMap<String, String>,
        caller_creation_flags: DWORD,
        caller_startup_info: &mut STARTUPINFOW,
        create_process_fn: F,
        progress: Option<P>,
    ) -> LayerResult<Self>
    where
        F: FnOnce(DWORD, LPVOID, &mut STARTUPINFOW) -> LayerResult<PROCESS_INFORMATION>,
        P: mirrord_progress::Progress,
    {
        let dll_path = std::env::var("MIRRORD_LAYER_FILE").map_err(LayerError::VarError)?;
        if !std::path::Path::new(&dll_path).exists() {
            return Err(LayerError::DllInjection(format!(
                "DLL file not found: {}",
                dll_path
            )));
        }

        let parent_event = LayerInitEvent::for_parent()?;

        // Determine the final environment: either caller's custom environment or parent environment
        // + mirrord vars Always create environment block since we need mirrord variables in
        // both cases
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

        // Calculate final creation flags (original + environment + suspended)
        let creation_flags = caller_creation_flags | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED;

        // Configure startup info for console handle inheritance - this ensures child processes
        // can inherit console handles for proper stdout/stderr output visibility
        caller_startup_info.dwFlags |= STARTF_USESTDHANDLES;
        caller_startup_info.hStdInput =
            Self::ensure_handle_inheritable(unsafe { GetStdHandle(STD_INPUT_HANDLE) })?;
        caller_startup_info.hStdOutput =
            Self::ensure_handle_inheritable(unsafe { GetStdHandle(STD_OUTPUT_HANDLE) })?;
        caller_startup_info.hStdError =
            Self::ensure_handle_inheritable(unsafe { GetStdHandle(STD_ERROR_HANDLE) })?;

        // Call the original function with processed parameters
        let process_info = create_process_fn(creation_flags, environment_ptr, caller_startup_info)?;

        let managed_process = Self {
            process_info,
            released: false,
            terminate_on_drop: true,
        };

        // The process is already created and suspended by the original call
        // Now we just need to inject the DLL and resume
        managed_process.inject_and_resume(&dll_path, &parent_event, progress)
    }

    /// Inject DLL into existing suspended process and resume execution
    fn inject_and_resume<P>(
        self,
        dll_path: &str,
        parent_event: &LayerInitEvent,
        progress: Option<P>,
    ) -> LayerResult<Self>
    where
        P: mirrord_progress::Progress,
    {
        let injector_process = InjectorOwnedProcess::from_pid(self.process_info.dwProcessId)
            .map_err(|_| LayerError::ProcessNotFound(self.process_info.dwProcessId))?;

        let syringe = Syringe::for_process(injector_process);
        let payload_path = OsString::from(dll_path);

        syringe
            .inject(payload_path)
            .map_err(|e| LayerError::DllInjection(format!("Failed to inject DLL: {}", e)))?;

        match parent_event.wait_for_signal(Some(LAYER_INIT_TIMEOUT_MS))? {
            true => {
                // Layer initialization successful - report ready!
                if let Some(mut progress) = progress {
                    progress.success(Some("Ready!"));
                }
            }
            false => {
                return Err(LayerError::ProcessSynchronization(format!(
                    "Layer initialization timed out after {}ms for process {}",
                    LAYER_INIT_TIMEOUT_MS, self.process_info.dwProcessId
                )));
            }
        }

        // Resume the main thread - ResumeThread returns the previous suspend count
        // A return value of u32::MAX (0xFFFFFFFF) indicates an error
        unsafe {
            let previous_suspend_count = ResumeThread(self.process_info.hThread);
            if previous_suspend_count == u32::MAX {
                let error = WindowsError::last_error();
                return Err(LayerError::WindowsProcessCreation(error));
            }
            tracing::debug!(
                "Thread resumed, previous suspend count: {}",
                previous_suspend_count
            );
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
            if GetExitCodeProcess(self.process_info.hProcess, &mut exit_code) == 0 {
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
