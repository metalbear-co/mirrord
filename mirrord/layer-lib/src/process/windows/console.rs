//! Windows console management utilities
//!
//! This module provides functions for managing Windows console settings,
//! particularly for enabling virtual terminal processing for ANSI escape sequences.

use std::env;

use mirrord_progress::MIRRORD_PROGRESS_ENV;
use tracing::{debug, warn};
use winapi::{
    shared::{
        minwindef::{DWORD, FALSE},
        winerror::ERROR_INVALID_PARAMETER,
    },
    um::{
        consoleapi::{GetConsoleMode, SetConsoleMode},
        handleapi::INVALID_HANDLE_VALUE,
        processenv::GetStdHandle,
        winbase::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE},
        wincon::{ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING},
    },
};

use crate::error::windows::WindowsError;

/// Enable virtual terminal processing for Windows console.
///
/// This allows the console to process ANSI escape sequences for colors and formatting,
/// which is essential for proper display of progress indicators and colored output.
pub fn enable_virtual_terminal_processing() {
    unsafe {
        for std_handle in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let handle = GetStdHandle(std_handle);
            if handle == INVALID_HANDLE_VALUE {
                let error = WindowsError::last_error();
                warn!(
                    handle = handle_name(std_handle),
                    error = ?error,
                    "standard handle is invalid"
                );
                continue;
            }

            let mut mode: DWORD = 0;
            if GetConsoleMode(handle, &mut mode) == FALSE {
                let error = WindowsError::last_error();
                debug!(
                    handle = handle_name(std_handle),
                    error = ?error,
                    "GetConsoleMode failed"
                );
                continue;
            }

            let new_mode = mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING | ENABLE_PROCESSED_OUTPUT;
            if SetConsoleMode(handle, new_mode) == FALSE {
                let error = WindowsError::last_error();
                if error == ERROR_INVALID_PARAMETER {
                    warn!(
                        handle = handle_name(std_handle),
                        "legacy console detected; disabling mirrord progress"
                    );

                    // Interactive progress is not supported in legacy consoles.
                    // Set the progress to "dumb" if not already set
                    if env::var_os(MIRRORD_PROGRESS_ENV).is_none() {
                        env::set_var(MIRRORD_PROGRESS_ENV, "dumb");
                    }

                    // No need to try other handles if we hit this error
                    return;
                }

                debug!(
                    handle = handle_name(std_handle),
                    error = ?error,
                    "SetConsoleMode failed"
                );
                continue;
            }

            debug!(
                handle = handle_name(std_handle),
                "enabled virtual terminal processing"
            );
        }
    }
}

fn handle_name(std_handle: DWORD) -> &'static str {
    match std_handle {
        STD_OUTPUT_HANDLE => "stdout",
        STD_ERROR_HANDLE => "stderr",
        _ => "unknown",
    }
}
