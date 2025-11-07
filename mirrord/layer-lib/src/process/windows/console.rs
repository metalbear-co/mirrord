//! Windows console management utilities
//!
//! This module provides functions for managing Windows console settings,
//! particularly for enabling virtual terminal processing for ANSI escape sequences.

use std::env;

use mirrord_progress::MIRRORD_PROGRESS_ENV;
use tracing::{error, info, warn};
use winapi::{
    shared::{
        minwindef::{DWORD, FALSE},
        winerror::ERROR_INVALID_PARAMETER,
    },
    um::{
        consoleapi::{GetConsoleMode, SetConsoleMode},
        handleapi::INVALID_HANDLE_VALUE,
        processenv::GetStdHandle,
        winbase::STD_OUTPUT_HANDLE,
        wincon::ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    },
};

use crate::error::windows::WindowsError;

/// Ensure modern consoles keep VT enabled so indicatif can redraw spinners in-place.
/// Legacy consoles cannot toggle this flag, so we fall back to dumb progress there.
pub fn ensure_vt_or_dumb_progress() {
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle == INVALID_HANDLE_VALUE {
            let error = WindowsError::last_error();
            error!(error = ?error, "standard stdout handle is invalid");
            return;
        }

        let mut mode: DWORD = 0;
        if GetConsoleMode(handle, &mut mode) == FALSE {
            let error = WindowsError::last_error();
            warn!(error = ?error, "GetConsoleMode for stdout failed");
            return;
        }

        if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
            // Console is already modern enough; no further probing needed.
            return;
        }

        // Enable VT on supported consoles so indicatif can move the cursor for spinner redraws.
        // Legacy consoles will reject this with ERROR_INVALID_PARAMETER; in that case we downgrade.
        let test_mode = mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        if SetConsoleMode(handle, test_mode) == FALSE {
            let error = WindowsError::last_error();
            if error == ERROR_INVALID_PARAMETER {
                info!("legacy console detected; dumb-ing up mirrord progress");

                // Interactive progress is not supported in legacy consoles.
                // Set the progress to "dumb" if not already set
                if env::var_os(MIRRORD_PROGRESS_ENV).is_none() {
                    env::set_var(MIRRORD_PROGRESS_ENV, "dumb");
                }
            } else {
                warn!(error = ?error, "SetConsoleMode probe on stdout failed");
            }
            return;
        }

        // VT is now enabled for this console session.
    }
}
