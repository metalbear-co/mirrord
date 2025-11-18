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
        winnt::{EVENT_ALL_ACCESS, HANDLE},
    },
};

use crate::error::{LayerError, LayerResult};

/// Environment variable name for passing initialization event names
pub const MIRRORD_LAYER_INIT_EVENT_NAME: &str = "MIRRORD_LAYER_INIT_EVENT_NAME";

/// Event role indicating whether this process created or opened the event
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventRole {
    /// This process created the event (parent role)
    Parent,
    /// This process opened an existing event (child role)
    Child,
}

/// Managed layer initialization event with RAII resource management.
///
/// Provides a clean, type-safe API for Windows event-based process synchronization
/// between parent and child processes during layer initialization.
///
/// # Usage Patterns
///
/// ## Parent Process (creates event for child)
/// ```rust
/// use mirrord_layer_lib::process::windows::sync::LayerInitEvent;
/// let mut env = std::collections::HashMap::new();
/// let event = LayerInitEvent::for_parent().expect("failed to create init event");
/// // Set environment variable for child
/// env.insert("MIRRORD_LAYER_INIT_EVENT_NAME", event.name().to_string());
/// // ... create child process ...
/// let success = event
///     .wait_for_signal(Some(30000))
///     .expect("Failed to wait for signal"); // 30 second timeout
/// // Event automatically cleaned up when dropped
/// ```
///
/// ## Child Process (opens parent's event)
/// ```rust, no_run
/// use mirrord_layer_lib::process::windows::sync::LayerInitEvent;
/// let event = LayerInitEvent::for_child().expect("failed to open parent init event");
/// // ... initialize layer ...
/// event
///     .signal_complete()
///     .expect("Failed to signal completion");
/// // Event automatically cleaned up when dropped
/// ```
pub struct LayerInitEvent {
    handle: HANDLE,
    name: String,
    role: EventRole,
}

impl LayerInitEvent {
    /// Create a new event for parent process with auto-generated unique name.
    ///
    /// The parent process should:
    /// 1. Create this event
    /// 2. Pass the event name to child via environment variable
    /// 3. Wait for child to signal completion
    pub fn for_parent() -> LayerResult<Self> {
        let event_name = generate_unique_event_name();
        Self::for_parent_with_name(event_name)
    }

    /// Create a new event for parent process with specific name.
    ///
    /// Use this when you need control over the event name, otherwise prefer `for_parent()`.
    pub fn for_parent_with_name(name: String) -> LayerResult<Self> {
        unsafe {
            let event_name_cstr = CString::new(name.clone())
                .map_err(|_| LayerError::GlobalAlreadyInitialized("Failed to create event name"))?;

            let event_handle = CreateEventA(
                // default security attributes
                std::ptr::null_mut(),
                // manual reset event
                TRUE,
                // initially not signaled
                FALSE,
                event_name_cstr.as_ptr(),
            );

            if event_handle.is_null() {
                return Err(LayerError::GlobalAlreadyInitialized(
                    "Failed to create parent init event",
                ));
            }

            tracing::debug!(
                event_name = %name,
                role = ?EventRole::Parent,
                "created layer initialization event"
            );

            Ok(Self {
                handle: event_handle,
                name,
                role: EventRole::Parent,
            })
        }
    }

    /// Open existing event for child process.
    ///
    /// The child process should:
    /// 1. Call this to open the parent's event (via MIRRORD_LAYER_INIT_EVENT_NAME env var)
    /// 2. Complete layer initialization
    /// 3. Call `signal_complete()` to notify parent
    ///
    /// # Errors
    /// Returns an error if no parent event name is provided or if the event cannot be opened.
    pub fn for_child() -> LayerResult<Self> {
        unsafe {
            // Check if parent process provided an event name
            let parent_event_name = std::env::var(MIRRORD_LAYER_INIT_EVENT_NAME)
                .map_err(|_| LayerError::ProcessSynchronization(
                    "Child process requires MIRRORD_LAYER_INIT_EVENT_NAME environment variable from parent".to_string()
                ))?;

            // Parent provided an event name - open the existing event
            let event_name_cstr = CString::new(parent_event_name.clone())
                .map_err(|_| LayerError::GlobalAlreadyInitialized("Failed to create event name"))?;

            let event_handle = OpenEventA(EVENT_ALL_ACCESS, FALSE, event_name_cstr.as_ptr());

            if event_handle.is_null() {
                return Err(LayerError::ProcessSynchronization(format!(
                    "Failed to open parent-provided event: {}",
                    parent_event_name
                )));
            }

            tracing::debug!(
                event_name = %parent_event_name,
                role = "child",
                "opened parent-provided layer initialization event"
            );

            Ok(Self {
                handle: event_handle,
                name: parent_event_name,
                role: EventRole::Child,
            })
        }
    }

    /// Get the event name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the Windows handle (for compatibility with existing APIs).
    pub fn handle(&self) -> HANDLE {
        self.handle
    }

    /// Check if this is a parent event (created by this process).
    pub fn is_parent(&self) -> bool {
        self.role == EventRole::Parent
    }

    /// Check if this is a child event (opened from parent).
    pub fn is_child(&self) -> bool {
        self.role == EventRole::Child
    }

    /// Signal that initialization is complete.
    ///
    /// This should be called by the child process after layer initialization is done.
    pub fn signal_complete(&self) -> LayerResult<()> {
        unsafe {
            if SetEvent(self.handle) == 0 {
                tracing::warn!(
                    event_name = %self.name,
                    role = ?self.role,
                    "failed to signal layer initialization event"
                );
                Err(LayerError::GlobalAlreadyInitialized(
                    "Failed to signal event",
                ))
            } else {
                tracing::debug!(
                    event_name = %self.name,
                    role = ?self.role,
                    "signaled layer initialization complete"
                );
                Ok(())
            }
        }
    }

    /// Wait for the event to be signaled with an optional timeout.
    ///
    /// This should be called by the parent process to wait for child initialization.
    ///
    /// # Returns
    /// - `Ok(true)` if the event was signaled
    /// - `Ok(false)` if timeout occurred
    /// - `Err(_)` if an error occurred during waiting
    pub fn wait_for_signal(&self, timeout_ms: Option<u32>) -> LayerResult<bool> {
        unsafe {
            let wait_timeout = timeout_ms.unwrap_or(INFINITE);
            let wait_result = WaitForSingleObject(self.handle, wait_timeout);

            match wait_result {
                WAIT_OBJECT_0 => {
                    tracing::debug!(
                        event_name = %self.name,
                        role = ?self.role,
                        "layer initialization event signaled successfully"
                    );
                    Ok(true)
                }
                WAIT_TIMEOUT => {
                    tracing::warn!(
                        event_name = %self.name,
                        role = ?self.role,
                        timeout_ms = wait_timeout,
                        "timeout waiting for layer initialization event"
                    );
                    Ok(false)
                }
                _ => {
                    let error_msg = format!(
                        "error waiting for layer initialization event '{}': result code {}",
                        self.name, wait_result
                    );
                    tracing::error!(
                        event_name = %self.name,
                        role = ?self.role,
                        result_code = wait_result,
                        "failed to wait for layer initialization event"
                    );
                    Err(LayerError::ProcessSynchronization(error_msg))
                }
            }
        }
    }
}

impl Drop for LayerInitEvent {
    fn drop(&mut self) {
        unsafe {
            if !self.handle.is_null() {
                CloseHandle(self.handle);
                tracing::debug!(
                    event_name = %self.name,
                    role = ?self.role,
                    "cleaned up layer initialization event"
                );
            }
        }
    }
}

/// Generate a unique event name for parent processes.
pub fn generate_unique_event_name() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let pid = std::process::id();
    let rand_part = timestamp & 0xFFFF; // Use lower 16 bits for some randomness

    format!("mirrord_layer_init_{}_{}", pid, rand_part)
}

/// Extract the initialization event name from environment variables.
pub fn get_env_event_name() -> Option<String> {
    std::env::var(MIRRORD_LAYER_INIT_EVENT_NAME).ok()
}
