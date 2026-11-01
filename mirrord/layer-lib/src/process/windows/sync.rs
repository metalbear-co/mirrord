//! Process synchronization between a launcher and the layer it loads, during layer startup.
//!
//! Two named manual-reset events per child, both named after the child's pid, so neither side
//! needs an environment variable: `mirrord_layer_init_{pid}` says the layer is ready, and
//! `mirrord_layer_init_failed_{pid}` says it gave up.
//!
//! The launcher holds both through [`ParentInitEvents`]. The layer claims the readiness one as a
//! [`ChildInitEvent`] while `DllMain` runs and sets it from its startup worker. A layer that fails
//! inside `DllMain` sets the failure one through [`signal_init_failure_to_parent`].
//!
//! The events themselves are [`NamedEvent`]s. This module owns only the protocol: the names, which
//! event wins when several things happen at once, and what each outcome means.
use std::{
    os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    time::Duration,
};

use utils_win::{
    error::WindowsError,
    event::{EventAccess, EventError, NamedEvent, Wake, wait_any},
    process::process_status,
};
use winapi::{
    shared::minwindef::FALSE,
    um::{
        handleapi::{CloseHandle, DuplicateHandle},
        processthreadsapi::{GetCurrentProcess, GetProcessId, OpenProcess},
        winnt::{DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, HANDLE, PROCESS_DUP_HANDLE},
    },
};

use super::{
    diagnostics::{SessionRole, session_role},
    execution::MIRRORD_LAYER_CHILD_PROCESS_PARENT_PID,
};
use crate::error::{LayerError, LayerResult};

/// Outcome of a parent wait that also watches the target process handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitWaitOutcome {
    /// The layer signaled initialization complete.
    Signaled,
    /// The layer signaled that it could not initialize.
    Failed,
    /// The target process exited before the layer reported ready.
    ProcessExited,
    /// None of the above happened within the timeout.
    TimedOut,
}

/// Which of the two events a child and its parent agree on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventKind {
    /// The layer finished initializing.
    Ready,
    /// The layer gave up. See [`signal_init_failure_to_parent`].
    Failed,
}

/// The launcher's side of a child's readiness handshake: both events, created before the layer
/// is injected and held until the launcher stops waiting.
pub struct ParentInitEvents {
    ready: NamedEvent,
    failed: NamedEvent,
    pid: u32,
}

/// The layer's side of the handshake: the readiness event, claimed inside `DllMain` and moved to
/// the startup worker that sets it.
pub struct ChildInitEvent(NamedEvent);

/// Keeps both readiness events alive in the target after the process that created them stops
/// waiting: an APC attach that returns to the debugger, or a launch whose caller keeps the child
/// suspended. The target owns the retained references until it exits.
///
/// Both events travel together. A layer that fails inside `DllMain` reports through the failure
/// one, so it must outlive the creator exactly as the readiness one does.
pub struct RemoteLayerInitEvent {
    process: OwnedHandle,
    /// Handle values in the target's handle table. Null for an event not duplicated there, or
    /// one whose lifetime was handed to the target.
    handles: [HANDLE; 2],
}

impl RemoteLayerInitEvent {
    /// Transfer event lifetime to the target after loading has been queued.
    pub fn retain(mut self) {
        self.handles = [std::ptr::null_mut(); 2];
    }
}

impl Drop for RemoteLayerInitEvent {
    fn drop(&mut self) {
        for handle in self.handles {
            if handle.is_null() {
                continue;
            }
            let mut local = std::ptr::null_mut();
            unsafe {
                if DuplicateHandle(
                    self.process.as_raw_handle().cast(),
                    handle,
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

impl ParentInitEvents {
    /// Creates both events for a child, named after its pid.
    ///
    /// Call this after creating the child suspended and before injecting the layer, then
    /// [`wait`](Self::wait) for the child to finish initializing.
    ///
    /// Both events carry a low mandatory label ([`EventAccess::LowIntegrity`]), so a child running
    /// at a lower integrity level than this process can still set them.
    ///
    /// An event that already exists is an error, not something to reuse. Its signaled state
    /// belongs to someone else: another session waiting on the same process, or a layer that is
    /// already loaded there and will not run `DllMain` again to signal a second time.
    pub fn create(child_pid: u32) -> LayerResult<Self> {
        // Both events exist before injection, because a layer that fails inside `DllMain` has
        // only the failure one to report with, and no chance to wait for the parent to catch up.
        let ready = create_fresh_event(EventKind::Ready, child_pid)?;
        let failed = create_fresh_event(EventKind::Failed, child_pid)?;

        tracing::debug!(child_pid, "created layer initialization events");

        Ok(Self {
            ready,
            failed,
            pid: child_pid,
        })
    }

    /// Duplicates both events into `process`, so they outlive this value.
    ///
    /// For a creator that stops waiting before the layer has loaded: an APC attach, which returns
    /// to the debugger so it can resume the target, and a launch whose caller asked for a
    /// suspended child. The layer opens the events by name inside `DllMain`, and a named event
    /// with no open handle is gone.
    ///
    /// # Errors
    ///
    /// When the target cannot be opened or an event cannot be duplicated into it. Whatever was
    /// already duplicated is released again.
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
        // Built before the first duplication, so an early return releases, through `Drop`,
        // whatever already crossed.
        let mut remote = RemoteLayerInitEvent {
            // SAFETY: a fresh handle this function owns.
            process: unsafe { OwnedHandle::from_raw_handle(raw.cast()) },
            handles: [std::ptr::null_mut(); 2],
        };
        for (event, target) in [&self.ready, &self.failed]
            .into_iter()
            .zip(remote.handles.iter_mut())
        {
            *target = event
                .duplicate_into(remote.process.as_handle())
                .map_err(|error| {
                    LayerError::ProcessSynchronization(format!(
                        "duplicate readiness event into target: {}",
                        os_error(&error)
                    ))
                })?;
        }
        Ok(remote)
    }

    /// Waits for the layer to report, or the target process to exit, whichever comes first.
    ///
    /// Watching the process handle turns a child that dies during layer initialization into a
    /// fast, explicit outcome instead of a full-timeout stall on an event nobody will ever signal.
    ///
    /// # Arguments
    ///
    /// * `process` - the child, with `SYNCHRONIZE` access.
    /// * `timeout_ms` - `None` waits for as long as it takes.
    ///
    /// # Errors
    ///
    /// When the wait itself fails.
    pub fn wait(
        &self,
        process: BorrowedHandle<'_>,
        timeout_ms: Option<u32>,
    ) -> LayerResult<InitWaitOutcome> {
        // The order is the priority order: a layer that both failed and died is a failure.
        let wake = wait_any(
            &[&self.ready, &self.failed],
            process,
            timeout_ms.map(|ms| Duration::from_millis(u64::from(ms))),
        )
        .map_err(|error| {
            LayerError::ProcessSynchronization(format!(
                "wait for the layer of process {}: {}",
                self.pid,
                os_error(&error)
            ))
        })?;

        let outcome = match wake {
            Wake::Event(0) => InitWaitOutcome::Signaled,
            // The failure event is the only other one waited on.
            Wake::Event(_) => InitWaitOutcome::Failed,
            Wake::ProcessExited => InitWaitOutcome::ProcessExited,
            Wake::TimedOut => InitWaitOutcome::TimedOut,
        };
        tracing::debug!(
            child_pid = self.pid,
            ?outcome,
            "layer initialization wait ended"
        );
        Ok(outcome)
    }
}

impl ChildInitEvent {
    /// Claims this process's readiness event, which its parent created before injecting the layer.
    ///
    /// The parent drops the event as soon as its wait ends, so the layer claims it inside
    /// `DllMain`, while the parent is certain to still hold it. Opening a named event loads no
    /// module, so it is safe under the loader lock.
    ///
    /// # Errors
    ///
    /// When no matching event exists, which means no parent is waiting.
    pub fn open() -> LayerResult<Self> {
        let pid = std::process::id();
        let Some(handle) = open_event(EventKind::Ready, pid) else {
            // The event is missing because the parent never created it or vanished before this
            // child opened it. Report our role and the parent's liveness so the log says which.
            let role = session_role();
            return Err(LayerError::ProcessSynchronization(format!(
                "No init event found for pid {pid} (role={}, {})",
                role.label(),
                parent_liveness(&role),
            )));
        };

        tracing::debug!(pid, "claimed the layer readiness event");
        Ok(Self(handle))
    }

    /// Tells the parent that the layer is ready.
    ///
    /// # Errors
    ///
    /// When the event cannot be set.
    pub fn signal_complete(&self) -> LayerResult<()> {
        self.0.set().map_err(|error| {
            LayerError::ProcessSynchronization(format!(
                "signal the layer readiness event: {}",
                os_error(&error)
            ))
        })?;
        tracing::debug!("signaled layer initialization complete");
        Ok(())
    }
}

/// Derive the event name both parent and child agree on.
fn event_name(kind: EventKind, child_pid: u32) -> String {
    match kind {
        EventKind::Ready => format!("mirrord_layer_init_{child_pid}"),
        EventKind::Failed => format!("mirrord_layer_init_failed_{child_pid}"),
    }
}

/// Creates a manual-reset event that must not exist yet. See [`ParentInitEvents::create`].
fn create_fresh_event(kind: EventKind, child_pid: u32) -> LayerResult<NamedEvent> {
    NamedEvent::create_fresh(event_name(kind, child_pid), EventAccess::LowIntegrity).map_err(
        |error| match error {
            EventError::Exists { .. } => LayerError::ProcessSynchronization(format!(
                "a mirrord layer event for process {child_pid} already exists: another mirrord \
                 session is waiting on this process, or the layer is already loaded in it"
            )),
            error => LayerError::ProcessSynchronization(format!(
                "create layer event {}: {}",
                event_name(kind, child_pid),
                os_error(&error)
            )),
        },
    )
}

/// Opens one of `pid`'s layer events with only the rights a child needs. See [`NamedEvent::open`].
///
/// # Returns
///
/// The event, or `None` when no such event exists or it cannot be opened.
fn open_event(kind: EventKind, pid: u32) -> Option<NamedEvent> {
    NamedEvent::open(&event_name(kind, pid)).ok().flatten()
}

/// The OS error behind an event failure, printed the way `std::io::Error` prints it, so the
/// handshake's messages keep their error code.
fn os_error(error: &EventError) -> std::io::Error {
    let source = match error {
        EventError::Create { source, .. }
        | EventError::Open { source, .. }
        | EventError::Set { source, .. }
        | EventError::Duplicate { source, .. }
        | EventError::Wait(source) => source,
        EventError::Exists { name } => {
            return std::io::Error::new(std::io::ErrorKind::AlreadyExists, name.clone());
        }
    };
    match source {
        WindowsError::Windows(code) => std::io::Error::from_raw_os_error(*code as i32),
        WindowsError::WinSock(code) => std::io::Error::from_raw_os_error(*code),
    }
}

/// Tells the waiting parent that this process's layer could not initialize.
///
/// The layer calls this from `DllMain` when the work it must finish before the target runs fails.
/// Nothing richer is possible there. Registering with the crash monitor opens a socket, which must
/// not happen under the loader lock, and a layer that failed there starts no thread of its own to
/// do it later. Opening a named event and setting it loads no module, which leaves it the one
/// signal available.
///
/// The parent turns this into a report. It has neither constraint.
///
/// No parent waiting is the normal case for a process mirrord did not create, and is only logged.
pub fn signal_init_failure_to_parent() {
    let name = event_name(EventKind::Failed, std::process::id());

    let Some(event) = open_event(EventKind::Failed, std::process::id()) else {
        tracing::debug!(
            event_name = %name,
            "no parent is waiting for this layer, so its failure is not reported"
        );
        return;
    };

    if event.set().is_ok() {
        tracing::info!(event_name = %name, "told the parent that layer initialization failed");
    } else {
        tracing::warn!(event_name = %name, "failed to signal the layer failure event");
    }
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use str_win::string_to_u16_buffer;
    use winapi::{
        shared::minwindef::{DWORD, TRUE},
        um::{
            synchapi::{CreateEventW, OpenEventW, WaitForSingleObject},
            winbase::WAIT_OBJECT_0,
            winnt::{EVENT_MODIFY_STATE, SID, SYNCHRONIZE},
        },
    };

    use utils_win::security::LowIntegrityLabel;

    use super::*;

    /// Opens an event by name with the rights [`open_event`] asks for.
    fn open_named(name: &str) -> Option<OwnedHandle> {
        let name = string_to_u16_buffer(name);
        let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE | SYNCHRONIZE, FALSE, name.as_ptr()) };
        (!handle.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(handle.cast()) })
    }

    /// Both event names are derived from this process's own pid, so the tests that use them cannot
    /// run at the same time as each other.
    static EVENT_NAMES: Mutex<()> = Mutex::new(());

    /// A layer that fails inside `DllMain` has only this signal, so it has to reach the parent.
    ///
    /// Both sides run here, named after this process's own pid. The two cases share one test
    /// because they are a sequence: the answer to "is a parent waiting" changes in the middle.
    #[test]
    fn a_child_tells_its_parent_that_the_layer_failed() {
        let _names = EVENT_NAMES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // No parent yet. A process mirrord did not create must get through this quietly.
        signal_init_failure_to_parent();

        let pid = std::process::id();
        let parent = ParentInitEvents::create(pid).expect("create the event pair");

        // A real handle to this process. It stays unsignaled while the test runs, which is what
        // makes the failure event the only thing the wait can return.
        let process = unsafe { OpenProcess(SYNCHRONIZE, FALSE, pid) };
        assert!(!process.is_null(), "open this process");
        let wait = || {
            parent
                .wait(
                    unsafe { BorrowedHandle::borrow_raw(process.cast()) },
                    Some(0),
                )
                .expect("wait on the pair")
        };

        assert_eq!(
            wait(),
            InitWaitOutcome::TimedOut,
            "a signal sent before the parent waited leaves nothing behind"
        );

        signal_init_failure_to_parent();
        let outcome = wait();
        unsafe { CloseHandle(process) };

        assert_eq!(
            outcome,
            InitWaitOutcome::Failed,
            "the parent must read a failure, not readiness and not an exit"
        );
    }

    /// A layer must claim the readiness event while the parent still holds it, not later.
    ///
    /// The parent creates the event immediately before it injects the layer, and drops it as soon
    /// as its wait ends. The layer claims it inside `DllMain` and hands the claim to its startup
    /// worker, which may signal only after the parent let go. The claim is what keeps the name
    /// alive for that signal; a worker that opened the event itself would race the drop.
    #[test]
    fn a_claimed_event_reaches_the_worker_after_the_parent_lets_go() {
        let _names = EVENT_NAMES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pid = std::process::id();

        let parent = ParentInitEvents::create(pid).expect("create the event pair");
        // What `DllMain` does: claim, then move the claim into the worker.
        let claimed = ChildInitEvent::open().expect("the child claims it in time");
        let (released, parent_released) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            parent_released.recv().expect("the parent lets go");
            claimed
                .signal_complete()
                .expect("a claimed event still signals after the parent let go");
            claimed
        });

        drop(parent);
        released.send(()).expect("worker");
        let claimed = worker.join().expect("worker");

        let late = open_event(EventKind::Ready, pid).expect("the claim keeps the event alive");
        assert_eq!(
            unsafe { WaitForSingleObject(late.as_raw_handle().cast(), 0) },
            WAIT_OBJECT_0,
            "the worker's signal reached the event"
        );
        drop(late);
        drop(claimed);

        assert!(
            ChildInitEvent::open().is_err(),
            "with no claim and no parent, the event is gone"
        );
    }

    /// A second waiter on the same process must be told, not handed someone else's state.
    #[test]
    fn an_existing_event_is_rejected() {
        let _names = EVENT_NAMES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pid = std::process::id();

        let first = ParentInitEvents::create(pid).expect("create the event pair");
        let second = ParentInitEvents::create(pid);
        assert!(
            second.is_err(),
            "an event that already exists belongs to another waiter"
        );

        drop(first);
        ParentInitEvents::create(pid).expect("a released pair can be created again");
    }

    /// A child at a lower integrity level than its parent must still be able to set both events.
    ///
    /// The test impersonates a low-integrity copy of its own token, which is what such a child
    /// runs with, and opens the events the way the layer does. An event created without the label
    /// is the control: it refuses the same open.
    #[test]
    fn a_low_integrity_child_can_signal_both_events() {
        let _names = EVENT_NAMES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pid = std::process::id();

        let parent = ParentInitEvents::create(pid).expect("create the event pair");
        let control_name = format!("mirrord_layer_test_unlabeled_{pid}");
        let control_wide = string_to_u16_buffer(&control_name);
        let control =
            unsafe { CreateEventW(std::ptr::null_mut(), TRUE, FALSE, control_wide.as_ptr()) };
        assert!(!control.is_null(), "create the control event");

        let (ready, failed, unlabeled) = low_integrity::run(|| {
            (
                open_event(EventKind::Ready, pid),
                open_event(EventKind::Failed, pid),
                open_named(&control_name),
            )
        });

        let (ready, failed, unlabeled) = (ready.is_some(), failed.is_some(), unlabeled.is_some());
        unsafe { CloseHandle(control) };
        drop(parent);

        assert!(ready, "the readiness event opens at low integrity");
        assert!(failed, "the failure event opens at low integrity");
        assert!(
            !unlabeled,
            "without the label, a low-integrity child is refused"
        );
    }

    /// Runs code on this thread under a low-integrity copy of the process token.
    mod low_integrity {
        use winapi::{
            shared::minwindef::LPVOID,
            um::{
                processthreadsapi::{GetCurrentProcess, OpenProcessToken, SetThreadToken},
                securitybaseapi::{
                    DuplicateTokenEx, GetLengthSid, RevertToSelf, SetTokenInformation,
                },
                winnt::{
                    MAXIMUM_ALLOWED, SE_GROUP_INTEGRITY, SID_AND_ATTRIBUTES, SecurityImpersonation,
                    TOKEN_DUPLICATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenImpersonation,
                    TokenIntegrityLevel,
                },
            },
        };

        use super::*;

        /// # Returns
        ///
        /// What `body` returned, after the thread is back on its own token.
        pub(super) fn run<T>(body: impl FnOnce() -> T) -> T {
            unsafe {
                let mut token = std::ptr::null_mut();
                assert_ne!(
                    OpenProcessToken(
                        GetCurrentProcess(),
                        TOKEN_DUPLICATE | TOKEN_QUERY,
                        &mut token
                    ),
                    0,
                    "open the process token"
                );

                let mut low = std::ptr::null_mut();
                let duplicated = DuplicateTokenEx(
                    token,
                    MAXIMUM_ALLOWED,
                    std::ptr::null_mut(),
                    SecurityImpersonation,
                    TokenImpersonation,
                    &mut low,
                );
                CloseHandle(token);
                assert_ne!(duplicated, 0, "duplicate the process token");

                let mut sid = LowIntegrityLabel::new().expect("label").sid();
                let mut label = TOKEN_MANDATORY_LABEL {
                    Label: SID_AND_ATTRIBUTES {
                        Sid: &mut sid as *mut SID as *mut _,
                        Attributes: SE_GROUP_INTEGRITY,
                    },
                };
                let size = std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as DWORD
                    + GetLengthSid(&mut sid as *mut SID as *mut _);
                assert_ne!(
                    SetTokenInformation(
                        low,
                        TokenIntegrityLevel,
                        &mut label as *mut TOKEN_MANDATORY_LABEL as LPVOID,
                        size,
                    ),
                    0,
                    "lower the copy's integrity level"
                );

                assert_ne!(SetThreadToken(std::ptr::null_mut(), low), 0, "impersonate");
                let result = body();
                RevertToSelf();
                CloseHandle(low);
                result
            }
        }
    }
}
