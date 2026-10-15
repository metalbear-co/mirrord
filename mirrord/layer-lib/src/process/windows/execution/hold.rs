//! Holding a launched program at its entry point until an IDE releases it, so a debugger can
//! attach before the program's first line runs (`mirrord up --debug`).
//!
//! The launcher creates a manual-reset event named after the program's pid, says so on stdout,
//! and waits on it with no timeout. The IDE attaches its debugger, then opens the event with
//! `EVENT_MODIFY_STATE` and sets it. Only the launcher's own child is held: the hold travels as a
//! launch argument, never through the environment the program and its descendants see.

use std::{
    io::Write,
    os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
};

use str_win::string_to_u16_buffer;
use utils_win::error::WindowsError;
use winapi::{
    shared::{
        minwindef::{FALSE, TRUE},
        winerror::ERROR_ALREADY_EXISTS,
    },
    um::{
        errhandlingapi::GetLastError,
        synchapi::{CreateEventW, WaitForMultipleObjects},
        winbase::{INFINITE, WAIT_OBJECT_0},
        winnt::HANDLE,
    },
};

/// Why a program could not be held for a debugger.
#[derive(Debug, thiserror::Error)]
pub enum DebugHoldError {
    #[error("failed to create the debug release event {event}")]
    CreateEvent {
        event: String,
        #[source]
        source: WindowsError,
    },

    #[error(
        "the debug release event {event} already exists, so another launcher may be holding a process with the same pid"
    )]
    EventExists { event: String },

    #[error("failed to announce the debug hold of process {pid} on stdout")]
    Announce {
        pid: u32,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to wait for the debug release of process {pid}")]
    Wait {
        pid: u32,
        #[source]
        source: WindowsError,
    },
}

/// How a hold ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HoldOutcome {
    /// The IDE set the release event, so the program may run.
    Released,
    /// The program ended while it was held, so there is nothing left to resume.
    ProcessExited,
}

/// The name of the event that releases the held process `pid`.
///
/// `Local\` keeps it in the user's session, where the IDE that releases it runs.
pub fn release_event_name(pid: u32) -> String {
    format!(r"Local\mirrord-debug-release-{pid}")
}

/// Holds the suspended process `pid` until its release event is set or it exits.
///
/// Prints `debug-hold pid=<pid> release=<event>` on stdout once the event exists, which
/// `mirrord up` prefixes with the service name for the IDE to match. The wait has no timeout: a
/// person is attaching a debugger. Whatever stops the launcher meanwhile (Ctrl-C, or `mirrord up`
/// ending its job) ends the launcher's process, and the kill-on-close job ends the held program
/// with it.
pub(super) fn hold(process: BorrowedHandle<'_>, pid: u32) -> Result<HoldOutcome, DebugHoldError> {
    let event_name = release_event_name(pid);
    let event = create_release_event(&event_name)?;

    // Only `mirrord exec` holds a program, never a hook inside the layer, so this stdout is the
    // CLI's.
    #[allow(clippy::disallowed_methods)]
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "debug-hold pid={pid} release={event_name}")
        .and_then(|()| stdout.flush())
        .map_err(|source| DebugHoldError::Announce { pid, source })?;
    drop(stdout);

    tracing::debug!(pid, %event_name, "debug hold: waiting for the release");
    let handles: [HANDLE; 2] = [event.as_raw_handle().cast(), process.as_raw_handle().cast()];
    let wait_result =
        unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), FALSE, INFINITE) };

    // `WaitForMultipleObjects` answers with the lowest signaled index, so a release that races
    // the program's exit counts as a release, and the resume then finds the process gone.
    match wait_result {
        WAIT_OBJECT_0 => Ok(HoldOutcome::Released),
        result if result == WAIT_OBJECT_0 + 1 => Ok(HoldOutcome::ProcessExited),
        _ => Err(DebugHoldError::Wait {
            pid,
            source: WindowsError::last_error(),
        }),
    }
}

/// Creates the manual-reset release event, unset.
///
/// One that already exists is refused rather than reused: it may already be set, which would
/// release this process before anyone attached.
fn create_release_event(event_name: &str) -> Result<OwnedHandle, DebugHoldError> {
    let wide = string_to_u16_buffer(event_name);
    let handle = unsafe { CreateEventW(std::ptr::null_mut(), TRUE, FALSE, wide.as_ptr()) };
    if handle.is_null() {
        return Err(DebugHoldError::CreateEvent {
            event: event_name.to_owned(),
            source: WindowsError::last_error(),
        });
    }
    let already_existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    // SAFETY: a fresh handle this function owns.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.cast()) };
    if already_existed {
        return Err(DebugHoldError::EventExists {
            event: event_name.to_owned(),
        });
    }
    Ok(handle)
}
