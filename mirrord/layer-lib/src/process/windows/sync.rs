/// Process synchronization utilities for Windows layer initialization.
///
/// Provides Windows-specific process synchronization between parent and child processes
/// using named events. Both sides derive the event name from the child's PID
/// (`mirrord_layer_init_{child_pid}`), so no environment variable handoff is needed.
use std::{
    ffi::CString,
    os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
};

use utils_win::process::process_status;
use winapi::{
    shared::{
        minwindef::{FALSE, TRUE},
        winerror::WAIT_TIMEOUT,
    },
    um::{
        handleapi::{CloseHandle, DuplicateHandle},
        processthreadsapi::{GetCurrentProcess, GetProcessId, OpenProcess},
        synchapi::{CreateEventA, OpenEventA, SetEvent, WaitForSingleObject},
        winbase::{INFINITE, WAIT_OBJECT_0},
        winnt::{
            DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, EVENT_ALL_ACCESS, HANDLE,
            PROCESS_DUP_HANDLE,
        },
    },
};

use super::{
    diagnostics::{SessionRole, session_role},
    execution::MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID,
};
use crate::error::{LayerError, LayerResult};

/// Event role indicating whether this process created or opened the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventRole {
    /// This process created the event (parent role).
    Parent,
    /// This process opened an existing event (child role).
    Child,
}

/// Managed layer initialization event with RAII resource management.
///
/// Provides a type-safe API for Windows event-based process synchronization
/// between parent and child processes during layer initialization.
///
/// Both sides agree on the event name using the child's PID
/// (`mirrord_layer_init_{child_pid}`), so no environment variable handoff is needed.
pub struct LayerInitEvent {
    handle: HANDLE,
    name: String,
    role: EventRole,
}

/// Keeps an APC readiness event alive after attach returns to the debugger.
/// The target owns the retained reference until it exits.
pub struct RemoteLayerInitEvent {
    process: OwnedHandle,
    handle: HANDLE,
    release_on_drop: bool,
}

impl RemoteLayerInitEvent {
    /// Transfer event lifetime to the target after loading has been queued.
    pub fn retain(mut self) {
        self.release_on_drop = false;
    }
}

impl Drop for RemoteLayerInitEvent {
    fn drop(&mut self) {
        if self.release_on_drop {
            let mut local = std::ptr::null_mut();
            unsafe {
                if DuplicateHandle(
                    self.process.as_raw_handle().cast(),
                    self.handle,
                    GetCurrentProcess(),
                    &mut local,
                    0,
                    FALSE,
                    DUPLICATE_CLOSE_SOURCE | DUPLICATE_SAME_ACCESS,
                ) != 0
                {
                    CloseHandle(local);
                }
            }
        }
    }
}

impl LayerInitEvent {
    /// Create a new event for the parent process, named after the child's PID.
    ///
    /// The parent should call this after creating the child process (suspended) and
    /// before injecting the layer DLL, then call [`wait_for_signal`](Self::wait_for_signal)
    /// to wait for the child to finish initialization.
    pub fn for_parent(child_pid: u32) -> LayerResult<Self> {
        let name = event_name(child_pid);

        unsafe {
            let name_cstr = CString::new(name.clone())
                .map_err(|_| LayerError::GlobalAlreadyInitialized("Failed to create event name"))?;

            let handle = CreateEventA(std::ptr::null_mut(), TRUE, FALSE, name_cstr.as_ptr());

            if handle.is_null() {
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
                handle,
                name,
                role: EventRole::Parent,
            })
        }
    }

    /// Duplicate the readiness event so an IDE can resume after attach exits.
    pub fn keep_alive_in_process(
        &self,
        process: BorrowedHandle<'_>,
    ) -> LayerResult<RemoteLayerInitEvent> {
        let pid = unsafe { GetProcessId(process.as_raw_handle().cast()) };
        if pid == 0 {
            return Err(LayerError::ProcessSynchronization(format!(
                "query event target pid: {}",
                std::io::Error::last_os_error()
            )));
        }
        // Injection handles need not carry the independent duplicate-handle right.
        let raw = unsafe { OpenProcess(PROCESS_DUP_HANDLE, FALSE, pid) };
        if raw.is_null() {
            return Err(LayerError::ProcessSynchronization(format!(
                "open event target: {}",
                std::io::Error::last_os_error()
            )));
        }
        let process = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
        let mut handle = std::ptr::null_mut();
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.handle,
                process.as_raw_handle().cast(),
                &mut handle,
                0,
                FALSE,
                DUPLICATE_SAME_ACCESS,
            )
        } == 0
        {
            return Err(LayerError::ProcessSynchronization(format!(
                "duplicate readiness event into target: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(RemoteLayerInitEvent {
            process,
            handle,
            release_on_drop: true,
        })
    }

    /// Open the existing event for the child (injected layer) process.
    ///
    /// Uses the current process's PID to derive the event name, matching what the
    /// parent created via [`for_parent`](Self::for_parent). After initialization,
    /// the child should call [`signal_complete`](Self::signal_complete).
    ///
    /// Returns `Err` if no matching event exists (i.e. no parent is waiting).
    pub fn for_child() -> LayerResult<Self> {
        let pid = std::process::id();
        let name = event_name(pid);

        unsafe {
            let name_cstr = CString::new(name.clone())
                .map_err(|_| LayerError::GlobalAlreadyInitialized("Failed to create event name"))?;

            let handle = OpenEventA(EVENT_ALL_ACCESS, FALSE, name_cstr.as_ptr());

            if handle.is_null() {
                // The event is missing because the parent never created it or vanished before this
                // child opened it. Report our role and the parent's liveness so the log says which.
                let role = session_role();
                return Err(LayerError::ProcessSynchronization(format!(
                    "No init event found for pid {pid} (role={}, {})",
                    role.label(),
                    parent_liveness(&role),
                )));
            }

            tracing::debug!(
                event_name = %name,
                role = "child",
                "opened layer initialization event"
            );

            Ok(Self {
                handle,
                name,
                role: EventRole::Child,
            })
        }
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

/// Derive the event name both parent and child agree on.
fn event_name(child_pid: u32) -> String {
    format!("mirrord_layer_init_{child_pid}")
}

/// Describes the parent process's identity and liveness for a `for_child` failure.
///
/// The parent pid comes from the session role. It falls back to the raw inheritance variable so a
/// malformed environment still surfaces what it can.
///
/// # Arguments
///
/// * `role` - the classified session role of this process.
///
/// # Returns
///
/// A short phrase such as `parent pid=19580 (node.exe) dead exit=0xc0000409`.
fn parent_liveness(role: &SessionRole) -> String {
    let parent_pid = match role {
        SessionRole::Child { parent_pid, .. } => Some(*parent_pid),
        _ => std::env::var(MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID)
            .ok()
            .and_then(|value| value.parse().ok()),
    };

    let Some(parent_pid) = parent_pid else {
        return "parent unknown (no parent pid in env)".to_owned();
    };

    let status = process_status(parent_pid);
    if status.alive {
        format!("parent pid={parent_pid} ({}) alive", status.name)
    } else if let Some(code) = status.exit_code {
        format!(
            "parent pid={parent_pid} ({}) dead exit={code:#x}",
            status.name
        )
    } else {
        format!("parent pid={parent_pid} gone (no handle)")
    }
}
