//! Unified Windows process execution with mandatory layer injection and initialization synchronization.
//!
//! This module provides a **unified execution system** that ensures ALL process creation flows:
//! 1. **Always inject the mirrord layer DLL**
//! 2. **Always wait for layer initialization** (prevents race conditions)  
//! 3. **Use consistent process creation lifecycle** across all contexts
//!
//! ## Key Architectural Principles
//!
//! ### Mandatory Layer Injection
//! - **ALL** process execution flows must inject the mirrord layer DLL
//! - **NO** process should be created without layer injection
//! - Prevents inconsistent behavior between different execution contexts
//!
//! ### Mandatory Initialization Synchronization  
//! - **ALL** parent processes must wait for child layer initialization
//! - Uses Windows named events for cross-process synchronization
//! - Prevents race conditions where parent returns before child layer is ready
//! - 30-second timeout with graceful degradation (log warning, continue)
//!
//! ### Unified Execution Flow
//! - `WindowsProcessExecutor` methods are the **single source of truth**
//! - All process creation flows delegate to the method-based implementation
//! - Consistent behavior across CLI, hooks, and direct API usage
//!
//! ## Core Components
//!
//! - **WindowsProcessExecutor**: Low-level executor with method-based approach (primary implementation)
//! - **ProcessExecutor**: High-level builder interface with std::process::Command compatibility
//! - **WindowsProcessConfig**: Internal configuration structure
//! - **WindowsProcessResult**: Process execution result with handles and exit code
//!
//! ## Usage Examples
//!
//! ### Recommended: High-Level Builder Interface
//! ```rust
//! use mirrord_layer_lib::process::windows::execution::ProcessExecutor;
//!
//! // For hook contexts (auto-configured)
//! let result = ProcessExecutor::new("notepad.exe")
//!     .arg("myfile.txt")
//!     .from_global_state()? // Auto-extracts proxy, layer_id, dll_path  
//!     .spawn()?; // Uses unified execution with init synchronization
//!
//! // For CLI contexts (manual configuration with stdio)
//! let result = ProcessExecutor::new("notepad.exe")
//!     .arg("myfile.txt")
//!     .stdin(Stdio::piped())   // std::process::Command methods via Deref
//!     .stdout(Stdio::piped())  // std::process::Command methods via Deref
//!     .stderr(Stdio::piped())  // std::process::Command methods via Deref
//!     .spawn_with_layer("mirrord_layer.dll")?; // Uses unified execution with init sync
//! ```
//!
//! ### CLI Usage with stdio redirection
//! ```rust
//! use mirrord_layer_lib::process::windows::execution::WindowsCliCommand;
//! 
//! let result = WindowsCliCommand::new(&binary_path)
//!     .args(&binary_args)      // ProcessExecutor methods via Deref
//!     .envs(env_vars)          // ProcessExecutor methods via Deref  
//!     .from_global_state()?    // Auto-configure from global state
//!     .stdout(Stdio::piped())  // CLI-specific stdio config
//!     .stderr(Stdio::piped())  // CLI-specific stdio config
//!     .spawn_with_layer(layer_path)?; // Uses unified execution internally
//! ```
//!
//! ### Direct API (Advanced Usage)
//! ```rust
//! use mirrord_layer_lib::process::windows::execution::{WindowsProcessExecutor, WindowsProcessConfig};
//!
//! let config = WindowsProcessConfig::cli("notepad.exe myfile.txt".to_string(), "mirrord_layer.dll".to_string())
//!     .with_env_vars(environment_vars);
//! let result = WindowsProcessExecutor::new(config).execute_and_wait()?;
//! ```

use std::{collections::HashMap, ffi::OsString, ptr};

use winapi::{
    shared::{minwindef::LPVOID, ntdef::HANDLE},
    um::{
        processthreadsapi::{CreateProcessW, PROCESS_INFORMATION, ResumeThread, STARTUPINFOW},
        minwinbase::LPSECURITY_ATTRIBUTES,
        processthreadsapi::LPPROCESS_INFORMATION,
        winnt::PHANDLE,
        synchapi::WaitForSingleObject,
        winbase::{INFINITE, WAIT_OBJECT_0, CREATE_SUSPENDED},
    },
};
use dll_syringe::{Syringe, process::OwnedProcess as InjectorOwnedProcess};
use str_win::string_to_u16_buffer;

use crate::error::{LayerError, LayerResult, windows::WindowsError};
use super::sync::{generate_child_event_name, wait_for_layer_initialization, MIRRORD_LAYER_INIT_EVENT_NAME};

pub mod pipes;
pub mod command;
pub mod environment;
pub mod process_info;

// Re-export the managed process type for convenience
pub use process_info::ManagedProcessInfo;

// Environment variable names for mirrord layer communication
pub const MIRRORD_AGENT_ADDR_ENV: &str = "MIRRORD_AGENT_ADDR";
pub const MIRRORD_LAYER_ID_ENV: &str = "MIRRORD_LAYER_ID";
pub const MIRRORD_LAYER_FILE_ENV: &str = "MIRRORD_LAYER_FILE";

/// Timeout for waiting for layer initialization in milliseconds (30 seconds)
pub const LAYER_INIT_TIMEOUT_MS: u32 = 30_000;

/// Function signature for CreateProcessInternalW Windows API call.
///
/// This internal Windows function is used by CreateProcess family functions
/// and is the ideal interception point for process creation hooks.
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

//

/// Configuration for Windows process creation with mirrord layer injection.
#[derive(Debug, Clone)]
pub struct WindowsProcessConfig {
    /// Optional application name (path to executable)
    pub application_name: Option<String>,
    /// Command line to execute (required)
    pub command_line: String,
    /// Working directory (optional)
    pub current_directory: Option<String>,
    /// Whether child process should inherit handles
    pub inherit_handles: bool,
    /// Process creation flags
    pub creation_flags: u32,
    /// Path to mirrord layer DLL
    pub dll_path: String,
    /// Proxy server address
    pub proxy_addr: String,
    /// Layer ID
    pub layer_id: u32,
    /// Environment variables
    pub environment: HashMap<String, String>,
    /// Original CreateProcessInternalW function (for hooks)
    pub original_create_process_fn: Option<&'static CreateProcessInternalWType>,
}

impl WindowsProcessConfig {
    /// Create a CLI configuration.
    pub fn cli(command_line: String, dll_path: String) -> Self {
        Self {
            application_name: None,
            command_line,
            current_directory: None,
            inherit_handles: false,
            creation_flags: 0,
            dll_path,
            proxy_addr: "127.0.0.1:0".to_string(),
            layer_id: 0,
            environment: HashMap::new(),
            original_create_process_fn: None,
        }
    }

    /// Add environment variables.
    pub fn with_env_vars(mut self, vars: HashMap<String, String>) -> Self {
        self.environment.extend(vars);
        self
    }

    /// Apply mirrord-specific environment variables.
    pub fn apply_mirrord_env(&mut self) {
        self.environment.insert(MIRRORD_AGENT_ADDR_ENV.to_string(), self.proxy_addr.clone());
        self.environment.insert(MIRRORD_LAYER_ID_ENV.to_string(), self.layer_id.to_string());
    }

    /// Get application name as wide string pointer for Windows API.
    pub fn application_name_ptr(&self) -> (Option<Vec<u16>>, winapi::shared::ntdef::LPCWSTR) {
        if let Some(ref name) = self.application_name {
            let wide = string_to_u16_buffer(name);
            let ptr = wide.as_ptr();
            (Some(wide), ptr)
        } else {
            (None, std::ptr::null())
        }
    }

    /// Get command line as mutable wide string for Windows API.
    pub fn command_line_wide(&self) -> Vec<u16> {
        string_to_u16_buffer(&self.command_line)
    }

    /// Get current directory as wide string pointer for Windows API.
    pub fn current_directory_ptr(&self) -> (Option<Vec<u16>>, winapi::shared::ntdef::LPCWSTR) {
        if let Some(ref dir) = self.current_directory {
            let wide = string_to_u16_buffer(dir);
            let ptr = wide.as_ptr();
            (Some(wide), ptr)
        } else {
            (None, std::ptr::null())
        }
    }

    /// Get creation flags with CREATE_SUSPENDED added.
    pub fn suspended_creation_flags(&self) -> u32 {
        self.creation_flags | CREATE_SUSPENDED
    }
}

impl Default for WindowsProcessConfig {
    fn default() -> Self {
        Self::cli(String::new(), "mirrord_layer.dll".to_string())
    }
}

//
// Shared Execution Functions
//

/// Result of Windows process execution  
pub struct WindowsProcessResult {
    /// Process ID of the created process
    pub process_id: u32,
    /// Thread ID of the main thread
    pub thread_id: u32,
    /// Handle to the process (caller responsible for cleanup)
    pub process_handle: HANDLE,
    /// Exit code if process completed, None if still running
    pub exit_code: Option<u32>,
}

/// Inject the mirrord layer DLL into a suspended process and wait for initialization.
/// 
/// This function combines DLL injection with mandatory initialization synchronization:
/// 1. Inject the DLL into the suspended process
/// 2. Wait for the layer initialization event (30 second timeout)
/// 3. Handle timeout gracefully with warnings (don't fail the process)
/// 
/// This ensures that we always wait for proper layer initialization before proceeding.
pub fn inject_layer_and_wait(process_info: &PROCESS_INFORMATION, dll_path: &str, init_event_name: &str) -> LayerResult<()> {
    // Step 1: Inject the DLL
    let injector_process = InjectorOwnedProcess::from_pid(process_info.dwProcessId)
        .map_err(|_| LayerError::ProcessNotFound(process_info.dwProcessId))?;

    let syringe = Syringe::for_process(injector_process);
    let payload_path = OsString::from(dll_path);

    syringe
        .inject(payload_path)
        .map_err(|e| LayerError::DllInjection(format!("Failed to inject DLL: {}", e)))?;

    tracing::debug!(
        pid = process_info.dwProcessId,
        dll_path = %dll_path,
        "DLL injection completed, waiting for layer initialization"
    );

    // Step 2: Wait for layer initialization event  
    // This is CRITICAL - we must always wait for the child layer to initialize
    // before returning control to prevent race conditions
    match wait_for_layer_initialization(init_event_name, Some(LAYER_INIT_TIMEOUT_MS)) {
        Ok(true) => {
            tracing::debug!(
                event_name = %init_event_name,
                pid = process_info.dwProcessId,
                "layer initialization completed successfully"
            );
        }
        Ok(false) => {
            tracing::warn!(
                event_name = %init_event_name,
                pid = process_info.dwProcessId,
                timeout_ms = LAYER_INIT_TIMEOUT_MS,
                "layer initialization timed out"
            );
            // Don't fail - process might still work, but log the warning
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                event_name = %init_event_name,
                pid = process_info.dwProcessId,
                "layer initialization synchronization failed"
            );
            // Don't fail - process might still work, but log the warning
        }
    }

    Ok(())
}

/// Resume the main thread of a suspended process.
pub fn resume_process(process_info: &PROCESS_INFORMATION) -> LayerResult<()> {
    unsafe {
        if ResumeThread(process_info.hThread) == u32::MAX {
            let error = WindowsError::last_error();
            return Err(LayerError::WindowsProcessCreation(error));
        }
    }
    Ok(())
}

/// Wait for process completion and return exit code (CLI-specific).
pub fn wait_for_process_completion(
    process_info: &PROCESS_INFORMATION,
) -> LayerResult<Option<u32>> {
    unsafe {
        let wait_result = WaitForSingleObject(process_info.hProcess, INFINITE);

        if wait_result != WAIT_OBJECT_0 {
            let error = WindowsError::last_error();
            return Err(LayerError::WindowsProcessCreation(error));
        }

        // Get exit code
        let mut exit_code: u32 = 0;
        let success = winapi::um::processthreadsapi::GetExitCodeProcess(
            process_info.hProcess,
            &mut exit_code as *mut u32,
        ) != 0;

        if success {
            Ok(Some(exit_code))
        } else {
            let error = WindowsError::last_error();
            tracing::warn!(
                error = %error,
                process_handle = ?process_info.hProcess,
                "Failed to get process exit code, returning None"
            );
            Ok(None)
        }
    }
}

/// Low-level Windows process executor with mirrord layer injection
/// 
/// Provides direct control over process execution lifecycle for advanced scenarios.
/// For most use cases, prefer the `ProcessExecutor` builder interface.
pub struct WindowsProcessExecutor {
    config: WindowsProcessConfig,
}

impl WindowsProcessExecutor {
    /// Create a new executor with the given configuration
    pub fn new(config: WindowsProcessConfig) -> Self {
        Self { config }
    }

    /// Execute the process with mirrord layer injection and mandatory initialization synchronization
    /// 
    /// Uses the unified execution flow that ALWAYS:
    /// 1. Creates init event for child synchronization
    /// 2. Injects mirrord layer DLL and waits for initialization (MANDATORY)
    /// 3. Returns without waiting for process completion (hook behavior)
    /// 
    /// This ensures all execution paths have consistent layer injection and proper synchronization.
    pub fn execute(mut self) -> LayerResult<WindowsProcessResult> {
        // Use method-based approach to eliminate parameter redundancy
        self.execute_with_unified_layer_injection_method(false)
    }

    /// Execute the process with mirrord layer injection and wait for completion
    /// 
    /// Uses the unified execution flow that ALWAYS:
    /// 1. Creates init event for child synchronization
    /// 2. Injects mirrord layer DLL and waits for initialization (MANDATORY)
    /// 3. Waits for process completion and returns exit code (CLI behavior)
    /// 
    /// This ensures consistent layer injection behavior across all execution contexts.
    pub fn execute_and_wait(mut self) -> LayerResult<WindowsProcessResult> {
        // Use method-based approach to eliminate parameter redundancy
        self.execute_with_unified_layer_injection_method(true)
    }

    /// Get a reference to the configuration
    pub fn config(&self) -> &WindowsProcessConfig {
        &self.config
    }

    /// Execute with unified layer injection (method version to avoid parameter redundancy)
    ///
    /// This is the method version of the standalone function that eliminates the need
    /// to pass all the configuration parameters separately.
    fn execute_with_unified_layer_injection_method(&mut self, wait_for_completion: bool) -> LayerResult<WindowsProcessResult> {
        // Apply mirrord environment variables
        self.config.apply_mirrord_env();
        
        // Step 1: Create initialization event for child process synchronization
        let init_event_name = generate_child_event_name();
        tracing::debug!(
            event_name = %init_event_name,
            "created initialization event for child process synchronization"
        );
        
        // Prepare environment with init event name
        let mut child_environment = self.config.environment.clone();
        child_environment.insert(MIRRORD_LAYER_INIT_EVENT_NAME.to_string(), init_event_name.clone());

        // Step 2: Create suspended process using appropriate API
        let raw_process_info = if let Some(_original_fn) = self.config.original_create_process_fn {
            tracing::debug!("using original CreateProcessInternalW function");
            self.create_suspended_process_with_original_api(&child_environment)?
        } else {
            tracing::debug!("using standard CreateProcessW function");
            self.create_suspended_process(&child_environment)?
        };

        // Wrap in RAII wrapper for automatic cleanup
        let managed_process = ManagedProcessInfo::new(raw_process_info);

        tracing::debug!(
            pid = managed_process.as_ref().dwProcessId,
            tid = managed_process.as_ref().dwThreadId,
            "created suspended process successfully"
        );

        // Step 3: Inject the mirrord layer DLL and wait for initialization
        if let Err(e) = inject_layer_and_wait(managed_process.as_ref(), &self.config.dll_path, &init_event_name) {
            tracing::error!(
                error = %e,
                pid = managed_process.as_ref().dwProcessId,
                dll_path = %self.config.dll_path,
                "layer DLL injection or initialization failed"
            );
            // Terminate and clean up using RAII wrapper
            managed_process.terminate_and_cleanup(1);
            return Err(e.into());
        }

        tracing::debug!(
            pid = managed_process.as_ref().dwProcessId,
            dll_path = %self.config.dll_path,
            "layer DLL injected and initialized successfully"
        );

        // Step 4: Resume the process
        if let Err(e) = resume_process(managed_process.as_ref()) {
            tracing::error!(
                error = %e,
                pid = managed_process.as_ref().dwProcessId,
                "process resume failed"
            );
            // Terminate and clean up using RAII wrapper
            managed_process.terminate_and_cleanup(1);
            return Err(e);
        }

        tracing::debug!(
            pid = managed_process.as_ref().dwProcessId,
            "process resumed successfully"
        );

        // Step 5: Handle wait behavior based on context
        let exit_code = if wait_for_completion {
            tracing::debug!(
                pid = managed_process.as_ref().dwProcessId,
                "waiting for process completion"
            );
            wait_for_process_completion(managed_process.as_ref())?
        } else {
            tracing::debug!(
                pid = managed_process.as_ref().dwProcessId,
                "process started successfully, not waiting for completion"
            );
            None
        };

        // Release the managed process - success case, let it continue running
        let process_info = managed_process.release();

        Ok(WindowsProcessResult {
            process_id: process_info.dwProcessId,
            thread_id: process_info.dwThreadId,
            process_handle: process_info.hProcess,
            exit_code,
        })
    }

    /// Create suspended process with original API (method version to avoid parameter redundancy)
    fn create_suspended_process_with_original_api(&self, environment: &HashMap<String, String>) -> LayerResult<PROCESS_INFORMATION> {
        use crate::process::windows::execution::environment::setup_windows_environment;

        let original_fn = self.config.original_create_process_fn
            .ok_or_else(|| LayerError::MissingFunctionPointer("CreateProcessInternalW".to_string()))?;

        tracing::debug!(
            application_name = ?self.config.application_name,
            command_line = %self.config.command_line,
            current_directory = ?self.config.current_directory,
            creation_flags = format_args!("{:#x}", self.config.creation_flags),
            inherit_handles = self.config.inherit_handles,
            "creating suspended process with original API"
        );

        unsafe {
            // Handle environment setup using helper function
            let (environment_ptr, actual_creation_flags, _windows_environment) = 
                setup_windows_environment(environment, self.config.suspended_creation_flags());

            // Get string pointers using config getters
            let (_app_name_wide, app_name_ptr) = self.config.application_name_ptr();
            let mut command_line_wide = self.config.command_line_wide();
            let (_current_dir_wide, current_dir_ptr) = self.config.current_directory_ptr();

            // Setup startup info
            let mut startup_info = STARTUPINFOW {
                cb: std::mem::size_of::<STARTUPINFOW>() as u32,
                ..std::mem::zeroed()
            };

            // Create process information structure
            let mut process_info: PROCESS_INFORMATION = std::mem::zeroed();

            // Call the original CreateProcessInternalW function with SUSPENDED flag
            let success = original_fn(
                ptr::null_mut(), // user_token
                app_name_ptr,
                command_line_wide.as_mut_ptr(),
                ptr::null_mut(), // process_attributes
                ptr::null_mut(), // thread_attributes
                self.config.inherit_handles.into(),
                actual_creation_flags,
                environment_ptr,
                current_dir_ptr,
                &mut startup_info,
                &mut process_info,
                ptr::null_mut(), // restricted_user_token
            ) != 0;

            if !success {
                let error = WindowsError::last_error();
                return Err(LayerError::WindowsProcessCreation(error));
            }

            Ok(process_info)
        }
    }

    /// Create suspended process (method version to avoid parameter redundancy) 
    fn create_suspended_process(&self, environment: &HashMap<String, String>) -> LayerResult<PROCESS_INFORMATION> {
        use crate::process::windows::execution::environment::setup_windows_environment;

        tracing::debug!(
            application_name = ?self.config.application_name,
            command_line = %self.config.command_line,
            current_directory = ?self.config.current_directory,
            creation_flags = format_args!("{:#x}", self.config.creation_flags),
            inherit_handles = self.config.inherit_handles,
            "creating suspended process"
        );

        unsafe {
            // Handle environment setup using helper function
            let (environment_ptr, actual_creation_flags, _windows_environment) = 
                setup_windows_environment(environment, self.config.suspended_creation_flags());

            // Get string pointers using config getters
            let (_app_name_wide, app_name_ptr) = self.config.application_name_ptr();
            let mut command_line_wide = self.config.command_line_wide();
            let (_current_dir_wide, current_dir_ptr) = self.config.current_directory_ptr();

            // Setup startup info
            let mut startup_info = STARTUPINFOW {
                cb: std::mem::size_of::<STARTUPINFOW>() as u32,
                ..std::mem::zeroed()
            };

            // Create process information structure
            let mut process_info: PROCESS_INFORMATION = std::mem::zeroed();

            // Call CreateProcessW with SUSPENDED flag
            let success = CreateProcessW(
                app_name_ptr,
                command_line_wide.as_mut_ptr(),
                ptr::null_mut(), // process_attributes
                ptr::null_mut(), // thread_attributes
                self.config.inherit_handles.into(),
                actual_creation_flags,
                environment_ptr,
                current_dir_ptr,
                &mut startup_info,
                &mut process_info,
            ) != 0;

            if !success {
                let error = WindowsError::last_error();
                return Err(LayerError::WindowsProcessCreation(error));
            }

            Ok(process_info)
        }
    }
}
