/// Process synchronization utilities for Windows layer initialization.
///
/// Provides Windows-specific process synchronization functionality for coordinating
/// layer initialization between parent and child processes using named events.

use std::{
    ffi::CString,
    time::{SystemTime, UNIX_EPOCH},
};

use winapi::{
    shared::{
        minwindef::{FALSE, TRUE},
        winerror::WAIT_TIMEOUT,
    },
    um::{
        handleapi::CloseHandle,
        synchapi::{CreateEventA, OpenEventA, SetEvent, WaitForSingleObject},
        winbase::{INFINITE, WAIT_OBJECT_0},
        winnt::{EVENT_ALL_ACCESS, HANDLE, SYNCHRONIZE},
    },
};

use crate::error::{LayerError, LayerResult};

/// Environment variable name for passing initialization event names
pub const MIRRORD_LAYER_INIT_EVENT_NAME: &str = "MIRRORD_LAYER_INIT_EVENT_NAME";

static mut INIT_EVENT_HANDLE: Option<HANDLE> = None;

/// Generate a unique event name for a child process.
pub fn generate_child_event_name() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let pid = std::process::id();
    let rand_part = timestamp & 0xFFFF; // Use lower 16 bits for some randomness

    format!("mirrord_layer_init_{}_{}", pid, rand_part)
}

/// Extract the initialization event name from environment variables
pub fn get_init_event_name() -> Option<String> {
    std::env::var(MIRRORD_LAYER_INIT_EVENT_NAME).ok()
}

/// Wait for the mirrord layer to complete initialization by waiting on a named event.
pub fn wait_for_layer_initialization(
    event_name: &str,
    timeout_ms: Option<u32>,
) -> Result<bool, String> {
    let event_name_cstr =
        CString::new(event_name).map_err(|_| "Failed to create event name string".to_string())?;

    unsafe {
        // Open the existing event
        let event_handle = OpenEventA(SYNCHRONIZE, FALSE, event_name_cstr.as_ptr());

        if event_handle.is_null() {
            return Err(format!(
                "Failed to open initialization event: {}",
                event_name
            ));
        }

        let wait_timeout = timeout_ms.unwrap_or(INFINITE);
        let wait_result = WaitForSingleObject(event_handle, wait_timeout);

        let _ = CloseHandle(event_handle);

        match wait_result {
            WAIT_OBJECT_0 => {
                tracing::debug!(
                    event_name = %event_name,
                    "layer initialization completed successfully"
                );
                Ok(true)
            }
            WAIT_TIMEOUT => {
                tracing::warn!(
                    event_name = %event_name,
                    timeout_ms = wait_timeout,
                    "timeout waiting for layer initialization"
                );
                Ok(false)
            }
            _ => Err(format!(
                "error waiting for initialization event '{}': result code {}",
                event_name, wait_result
            )),
        }
    }
}

/// Create or open a named event for initialization synchronization.
pub fn create_init_event() -> LayerResult<String> {
    unsafe {
        // Check if parent process provided an event name
        if let Ok(parent_event_name) = std::env::var(MIRRORD_LAYER_INIT_EVENT_NAME) {
            // Parent provided an event name - open the existing event
            let event_name_cstr = CString::new(parent_event_name.clone())
                .map_err(|_| LayerError::GlobalAlreadyInitialized("Failed to create event name"))?;

            let event_handle = OpenEventA(EVENT_ALL_ACCESS, FALSE, event_name_cstr.as_ptr());

            if event_handle.is_null() {
                tracing::warn!(
                    event_name = %parent_event_name,
                    "failed to open parent-provided event, creating new one"
                );
                // Fall through to create a new event
            } else {
                INIT_EVENT_HANDLE = Some(event_handle);
                tracing::debug!(
                    event_name = %parent_event_name,
                    "opened parent-provided initialization event"
                );
                return Ok(parent_event_name);
            }
        }

        // No parent event or failed to open - create our own event
        let current_pid = std::process::id();
        let event_name = format!("mirrord_layer_init_{}", current_pid);
        let event_name_cstr = CString::new(event_name.clone())
            .map_err(|_| LayerError::GlobalAlreadyInitialized("Failed to create event name"))?;

        let event_handle = CreateEventA(
            std::ptr::null_mut(), // default security attributes
            TRUE,                 // manual reset event
            FALSE,                // initially not signaled
            event_name_cstr.as_ptr(),
        );

        if event_handle.is_null() {
            return Err(LayerError::GlobalAlreadyInitialized(
                "Failed to create init event",
            ));
        }

        INIT_EVENT_HANDLE = Some(event_handle);
        tracing::debug!(
            event_name = %event_name,
            "created new initialization event"
        );
        Ok(event_name)
    }
}

/// Signal that initialization is complete.
pub fn signal_init_complete() {
    unsafe {
        if let Some(event_handle) = INIT_EVENT_HANDLE {
            if SetEvent(event_handle) == 0 {
                tracing::warn!("failed to signal initialization event");
            } else {
                tracing::debug!("signaled initialization complete");
            }
        }
    }
}

/// Clean up the initialization event.
pub fn cleanup_init_event() {
    unsafe {
        if let Some(event_handle) = INIT_EVENT_HANDLE {
            CloseHandle(event_handle);
            INIT_EVENT_HANDLE = None;
            tracing::debug!("cleaned up initialization event");
        }
    }
}


