//! Windows console management utilities
//!
//! This module provides functions for managing Windows console settings,
//! particularly for enabling virtual terminal processing for ANSI escape sequences.

use winapi::{
    shared::minwindef::DWORD,
    um::{
        consoleapi::{GetConsoleMode, SetConsoleMode},
        handleapi::INVALID_HANDLE_VALUE,
        processenv::GetStdHandle,
        winbase::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE},
        wincon::ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    },
};

/// Enable virtual terminal processing for Windows console.
///
/// This allows the console to process ANSI escape sequences for colors and formatting,
/// which is essential for proper display of progress indicators and colored output.
pub fn enable_virtual_terminal_processing() {
    unsafe {
        // Enable for stdout
        let stdout_handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if stdout_handle != INVALID_HANDLE_VALUE {
            let mut mode: DWORD = 0;
            if GetConsoleMode(stdout_handle, &mut mode) != 0 {
                let new_mode = mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
                SetConsoleMode(stdout_handle, new_mode);
            }
        }

        // Enable for stderr
        let stderr_handle = GetStdHandle(STD_ERROR_HANDLE);
        if stderr_handle != INVALID_HANDLE_VALUE {
            let mut mode: DWORD = 0;
            if GetConsoleMode(stderr_handle, &mut mode) != 0 {
                let new_mode = mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
                SetConsoleMode(stderr_handle, new_mode);
            }
        }
    }
}
