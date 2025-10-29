//! Utility functions for Windows-specific process operations

use mirrord_layer_lib::graceful_exit;
use winapi::{
    shared::minwindef::FALSE,
    um::{
        handleapi::CloseHandle,
        processthreadsapi::{GetCurrentProcess, OpenProcessToken},
        securitybaseapi::GetTokenInformation,
        winnt::{TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
    },
};

use crate::hooks::socket::utils::ERROR_SUCCESS_I32;

/// Check if the current process is running with elevated UAC privileges
///
/// This function queries the process token to determine if the current process
/// has been launched with elevated privileges (Administrator/UAC elevation).
///
/// # Returns
///
/// `true` if the process is running with elevated privileges, `false` otherwise.
/// Returns `false` if the elevation status cannot be determined.
pub fn is_process_elevated() -> bool {
    unsafe {
        let process = GetCurrentProcess();
        let mut token_handle = std::ptr::null_mut();

        // Open the process token
        if OpenProcessToken(process, TOKEN_QUERY, &mut token_handle) == 0 {
            tracing::debug!("is_process_elevated -> failed to open process token");
            return false;
        }

        let mut elevation = TOKEN_ELEVATION {
            TokenIsElevated: false.into(),
        };
        let mut return_length = 0u32;

        // Get token elevation information
        let result = GetTokenInformation(
            token_handle,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut return_length,
        );

        // Close the token handle
        CloseHandle(token_handle);

        if result != ERROR_SUCCESS_I32 {
            tracing::debug!("is_process_elevated -> failed to get token information");
            return false;
        }

        let is_elevated = elevation.TokenIsElevated != FALSE as u32;
        tracing::debug!("is_process_elevated -> process elevated: {}", is_elevated);
        is_elevated
    }
}

/// Check if the current process has sufficient privileges for the operation.
///
/// If the process is not elevated, triggers a graceful_exit with the provided reason.
///
/// # Arguments
///
/// * `reason` - The reason message to display if privileges are insufficient
pub fn require_elevation(reason: &str) {
    if !is_process_elevated() {
        graceful_exit!("{}", reason);
    }
}
