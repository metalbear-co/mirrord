//! Debug functionality for Windows process execution.
//!
//! This module provides functionality for controlling debugger attachment behavior
//! through the `MIRRORD_LAYER_WAIT_FOR_DEBUGGER` environment variable.
//!
//! ## Environment Variable Behavior
//!
//! The `MIRRORD_LAYER_WAIT_FOR_DEBUGGER` environment variable controls when child processes
//! should wait for debugger attachment during DLL initialization:
//!
//! - **Empty or unset**: No debugger wait (default behavior)
//! - **"1"**: Wait for debugger on all processes
//! - **Process name**: Wait only for processes whose executable name matches the value
//!   (case-insensitive)
//!
//! ## Examples
//!
//! ```bash
//! # Wait for debugger on all child processes
//! set MIRRORD_LAYER_WAIT_FOR_DEBUGGER=1
//!
//! # Wait for debugger only on Python processes  
//! set MIRRORD_LAYER_WAIT_FOR_DEBUGGER=python
//!
//! # Wait for debugger only on Node.js processes
//! set MIRRORD_LAYER_WAIT_FOR_DEBUGGER=node
//!
//! # No debugger wait (default)
//! set MIRRORD_LAYER_WAIT_FOR_DEBUGGER=
//! ```
//!
//! ## Architecture
//!
//! The debugger wait functionality follows a two-stage architecture:
//!
//! 1. **Parent Process (layer-win/hooks/process/ops.rs)**: Always propagates the
//!    `MIRRORD_LAYER_WAIT_FOR_DEBUGGER` environment variable unchanged to child processes during
//!    DLL injection.
//!
//! 2. **Child Process (this module)**: Each child process DLL determines independently whether it
//!    should wait for debugger attachment based on its own process name and the environment
//!    variable value.
//!
//! This separation ensures that filtering decisions are made where they belong (in the
//! child process) rather than trying to predict in the parent which children should wait.

use std::env;

use mirrord_config::MIRRORD_LAYER_WAIT_FOR_DEBUGGER;

/// Determines if the current process should wait for debugger attachment.
///
/// This function checks the `MIRRORD_LAYER_WAIT_FOR_DEBUGGER` environment variable
/// and decides whether the current process should pause during DLL initialization
/// to allow debugger attachment.
///
/// # Return Value
///
/// Returns `true` if the process should wait for debugger, `false` otherwise.
///
/// # Behavior
///
/// - **Value "1"**: Always returns `true` (wait for all processes)
/// - **Process name**: Returns `true` if current executable name matches (case-insensitive)
/// - **Empty/unset**: Returns `false` (no debugger wait)
///
/// # Examples
///
/// ```rust
/// use mirrord_layer_lib::process::windows::execution::debug::should_wait_for_debugger;
///
/// // For a process named "python.exe" with MIRRORD_LAYER_WAIT_FOR_DEBUGGER=python
/// let should_wait = should_wait_for_debugger(); // Returns true
///
/// // For any process with MIRRORD_LAYER_WAIT_FOR_DEBUGGER=1
/// let should_wait = should_wait_for_debugger(); // Returns true
///
/// // For any process with MIRRORD_LAYER_WAIT_FOR_DEBUGGER unset
/// let should_wait = should_wait_for_debugger(); // Returns false
/// ```
pub fn should_wait_for_debugger() -> bool {
    let wait_debugger = match env::var(MIRRORD_LAYER_WAIT_FOR_DEBUGGER) {
        Ok(value) => value,
        Err(_) => return false,
    };

    if wait_debugger == "1" {
        // Wait for debugger on all processes
        true
    } else if !wait_debugger.is_empty() {
        // Process name filter - get current executable name
        let current_exe = get_current_process_name().to_lowercase();
        let filter_name = wait_debugger.to_lowercase();
        let should_wait = current_exe == filter_name;

        if should_wait {
            eprintln!(
                "mirrord: Process '{}' matches debugger filter '{}', waiting for debugger",
                current_exe, wait_debugger
            );
        }

        should_wait
    } else {
        // Empty value means no debugger wait
        false
    }
}

/// The current process's executable name, without extension.
pub use utils_win::process::get_current_process_name;

/// Checks if debugger waiting is enabled for any process.
///
/// This is a utility function that checks if the `MIRRORD_LAYER_WAIT_FOR_DEBUGGER`
/// environment variable is set to any value that would cause some processes to wait.
///
/// # Return Value
///
/// Returns `true` if debugger waiting is enabled (either globally or for specific processes),
/// `false` if it's disabled or unset.
///
/// # Examples
///
/// ```rust
/// use mirrord_layer_lib::process::windows::execution::debug::is_debugger_wait_enabled;
///
/// // With MIRRORD_LAYER_WAIT_FOR_DEBUGGER=1
/// let enabled = is_debugger_wait_enabled(); // Returns true
///
/// // With MIRRORD_LAYER_WAIT_FOR_DEBUGGER=python
/// let enabled = is_debugger_wait_enabled(); // Returns true
///
/// // With MIRRORD_LAYER_WAIT_FOR_DEBUGGER unset
/// let enabled = is_debugger_wait_enabled(); // Returns false
/// ```
pub fn is_debugger_wait_enabled() -> bool {
    env::var(MIRRORD_LAYER_WAIT_FOR_DEBUGGER)
        .map(|value| !value.is_empty())
        .unwrap_or(false)
}

/// Formats the debugger wait configuration for display purposes.
///
/// This function returns a human-readable description of the current debugger
/// wait configuration based on the environment variable value.
///
/// # Return Value
///
/// Returns a `String` describing the current configuration.
///
/// # Examples
///
/// ```rust
/// use mirrord_layer_lib::process::windows::execution::debug::format_debugger_config;
///
/// // With MIRRORD_LAYER_WAIT_FOR_DEBUGGER=1
/// let config = format_debugger_config(); // Returns "all processes"
///
/// // With MIRRORD_LAYER_WAIT_FOR_DEBUGGER=python
/// let config = format_debugger_config(); // Returns "processes matching 'python'"
///
/// // With MIRRORD_LAYER_WAIT_FOR_DEBUGGER unset
/// let config = format_debugger_config(); // Returns "disabled"
/// ```
pub fn format_debugger_config() -> String {
    match env::var(MIRRORD_LAYER_WAIT_FOR_DEBUGGER) {
        Ok(value) if value == "1" => "all processes".to_string(),
        Ok(value) if !value.is_empty() => format!("processes matching '{}'", value),
        _ => "disabled".to_string(),
    }
}
