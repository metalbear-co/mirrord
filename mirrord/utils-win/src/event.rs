//! Named Windows events that one process creates and another sets, the one primitive behind both
//! mirrord's layer readiness handshake (`mirrord_layer_lib::process::windows::sync`) and its debug
//! hold (`mirrord_layer_lib::process::hold`).
//!
//! A launcher creates the event unset, under a name the other side can derive or read, and waits
//! on it together with the process it launched, so a process that dies turns into an outcome
//! instead of a stall. The other side opens the event by name and sets it.
//!
//! This module owns no protocol. Each caller keeps its own names and its own meaning for each
//! event; this module only creates, opens, sets and waits.

use std::{
    os::windows::io::{
        AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle, RawHandle,
    },
    time::Duration,
};

use str_win::string_to_u16_buffer;
use winapi::{
    shared::{
        minwindef::{DWORD, FALSE, TRUE},
        winerror::{ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, WAIT_TIMEOUT},
    },
    um::{
        errhandlingapi::GetLastError,
        handleapi::DuplicateHandle,
        minwinbase::SECURITY_ATTRIBUTES,
        processthreadsapi::GetCurrentProcess,
        synchapi::{CreateEventW, OpenEventW, SetEvent, WaitForMultipleObjects},
        winbase::{INFINITE, WAIT_OBJECT_0},
        winnt::{DUPLICATE_SAME_ACCESS, EVENT_MODIFY_STATE, HANDLE, SYNCHRONIZE},
    },
};

use crate::{error::WindowsError, security::LowIntegrityLabel};

/// Why an operation on a named event failed.
#[derive(Debug, thiserror::Error)]
pub enum EventError {
    #[error("failed to create event {name}")]
    Create {
        name: String,
        #[source]
        source: WindowsError,
    },

    /// Its signaled state belongs to someone else, so it is never reused.
    #[error("event {name} already exists")]
    Exists { name: String },

    #[error("failed to open event {name}")]
    Open {
        name: String,
        #[source]
        source: WindowsError,
    },

    #[error("failed to set event {name}")]
    Set {
        name: String,
        #[source]
        source: WindowsError,
    },

    #[error("failed to duplicate event {name} into another process")]
    Duplicate {
        name: String,
        #[source]
        source: WindowsError,
    },

    #[error("failed to wait on events")]
    Wait(#[source] WindowsError),
}

/// A named, manual-reset, one-shot Windows event: created unset by one process, set by another.
pub struct NamedEvent {
    handle: OwnedHandle,
    name: String,
}

/// Who may set the event besides processes at the creator's integrity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventAccess {
    /// The creator's default label: only processes at its integrity level or above.
    Default,
    /// A low mandatory label, so a process at a lower integrity level can set it too. See
    /// [`LowIntegrityLabel`].
    LowIntegrity,
}

/// What ended a [`wait_any`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// The event at this index of the slice waited on was set.
    Event(usize),
    /// The process waited on exited.
    ProcessExited,
    /// Nothing happened within the timeout.
    TimedOut,
}

impl NamedEvent {
    /// Creates the event, unset.
    ///
    /// One that already exists is refused ([`EventError::Exists`]) rather than reused: it may
    /// already be set, or another launcher owns it and waits on it.
    ///
    /// When [`EventAccess::LowIntegrity`] cannot be honoured because Windows refuses to build the
    /// label, the event gets the default label and a warning is logged: it still serves every
    /// process at the creator's own level.
    pub fn create_fresh(name: String, access: EventAccess) -> Result<Self, EventError> {
        let mut label = match access {
            EventAccess::Default => None,
            EventAccess::LowIntegrity => {
                let label = LowIntegrityLabel::new();
                if label.is_none() {
                    tracing::warn!(
                        event_name = %name,
                        "could not build a low integrity label for the event, so a process at a lower integrity level cannot set it"
                    );
                }
                label
            }
        };
        let mut attributes = label.as_mut().map(|label| label.attributes());
        let attributes_ptr = attributes
            .as_mut()
            .map_or(std::ptr::null_mut(), |attributes| {
                attributes as *mut SECURITY_ATTRIBUTES
            });

        let wide = string_to_u16_buffer(&name);
        // SAFETY: `attributes_ptr` is null or points into `attributes`, whose descriptor points
        // into `label`, both alive for this call.
        let handle = unsafe { CreateEventW(attributes_ptr, TRUE, FALSE, wide.as_ptr()) };
        if handle.is_null() {
            return Err(EventError::Create {
                name,
                source: WindowsError::last_error(),
            });
        }
        let already_existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        // SAFETY: a fresh handle this function owns.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle.cast()) };
        if already_existed {
            return Err(EventError::Exists { name });
        }
        Ok(Self { handle, name })
    }

    /// Opens an existing event with only the rights the setting side needs: setting it, and
    /// waiting on it.
    ///
    /// Asking for no more than that is what lets a process at a lower integrity level open an
    /// event created with [`EventAccess::LowIntegrity`].
    ///
    /// # Returns
    ///
    /// `None` when no event has this name.
    pub fn open(name: &str) -> Result<Option<Self>, EventError> {
        let wide = string_to_u16_buffer(name);
        let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE | SYNCHRONIZE, FALSE, wide.as_ptr()) };
        if handle.is_null() {
            let source = WindowsError::last_error();
            return if source == ERROR_FILE_NOT_FOUND {
                Ok(None)
            } else {
                Err(EventError::Open {
                    name: name.to_owned(),
                    source,
                })
            };
        }
        Ok(Some(Self {
            // SAFETY: a fresh handle this function owns.
            handle: unsafe { OwnedHandle::from_raw_handle(handle.cast()) },
            name: name.to_owned(),
        }))
    }

    /// Sets the event, which wakes every waiter, now and later, until it is gone.
    pub fn set(&self) -> Result<(), EventError> {
        if unsafe { SetEvent(self.handle.as_raw_handle().cast()) } == 0 {
            return Err(EventError::Set {
                name: self.name.clone(),
                source: WindowsError::last_error(),
            });
        }
        Ok(())
    }

    /// Duplicates the event into `process`, so it lives for as long as that process holds it,
    /// even after this value drops.
    ///
    /// # Arguments
    ///
    /// * `process` - the target, with `PROCESS_DUP_HANDLE` access.
    ///
    /// # Returns
    ///
    /// The handle's value in the target's handle table, which only the target can use. Closing
    /// it is the target's business, or that of whoever duplicates it back with
    /// `DUPLICATE_CLOSE_SOURCE`.
    pub fn duplicate_into(&self, process: BorrowedHandle<'_>) -> Result<HANDLE, EventError> {
        let mut target = std::ptr::null_mut();
        let duplicated = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.handle.as_raw_handle().cast(),
                process.as_raw_handle().cast(),
                &mut target,
                0,
                FALSE,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if duplicated == 0 {
            return Err(EventError::Duplicate {
                name: self.name.clone(),
                source: WindowsError::last_error(),
            });
        }
        Ok(target)
    }

    /// The name the event was created or opened under.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl AsHandle for NamedEvent {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }
}

impl AsRawHandle for NamedEvent {
    fn as_raw_handle(&self) -> RawHandle {
        self.handle.as_raw_handle()
    }
}

/// Waits until one of `events` is set, `process` exits, or `timeout` passes.
///
/// Watching the process turns one that dies before anyone sets an event into a fast, explicit
/// outcome, instead of a wait for a signal nobody will ever send.
///
/// The order of `events` is a priority order: when several are set, or one is set as the process
/// exits, the lowest index wins, and any event wins over the exit. `WaitForMultipleObjects`
/// answers with the lowest signaled index. It takes at most `MAXIMUM_WAIT_OBJECTS` (64) handles,
/// the process included.
///
/// # Arguments
///
/// * `process` - with `SYNCHRONIZE` access.
/// * `timeout` - `None` waits for as long as it takes.
pub fn wait_any(
    events: &[&NamedEvent],
    process: BorrowedHandle<'_>,
    timeout: Option<Duration>,
) -> Result<Wake, EventError> {
    let handles: Vec<HANDLE> = events
        .iter()
        .map(|event| event.handle.as_raw_handle().cast())
        .chain(std::iter::once(process.as_raw_handle().cast()))
        .collect();
    // A finite timeout too long for the API waits as long as it can, never forever.
    let timeout_ms = timeout.map_or(INFINITE, |timeout| {
        DWORD::try_from(timeout.as_millis()).map_or(INFINITE - 1, |ms| ms.min(INFINITE - 1))
    });
    let result = unsafe {
        WaitForMultipleObjects(handles.len() as DWORD, handles.as_ptr(), FALSE, timeout_ms)
    };

    let index = result.wrapping_sub(WAIT_OBJECT_0) as usize;
    if index < events.len() {
        Ok(Wake::Event(index))
    } else if index == events.len() {
        Ok(Wake::ProcessExited)
    } else if result == WAIT_TIMEOUT {
        Ok(Wake::TimedOut)
    } else {
        Err(EventError::Wait(WindowsError::last_error()))
    }
}
