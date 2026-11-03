//! The Windows release gate: a named event the IDE sets once its debugger is attached.
//!
//! `mirrord exec` holds the program it created suspended, so the wait also watches that process:
//! one ended while held (the IDE's Stop, say) ends the hold instead of leaving it waiting forever.

use std::os::windows::io::BorrowedHandle;

pub use utils_win::event::EventError as GateError;
use utils_win::event::{EventAccess, NamedEvent, Wake, wait_any};
use uuid::Uuid;

use crate::process::hold::{HoldError, HoldOutcome, ReleaseGate};

/// A release event, created unset, that the IDE opens with `EVENT_MODIFY_STATE` and sets.
pub struct Gate {
    event: NamedEvent,
    pid: u32,
}

impl ReleaseGate for Gate {
    /// The suspended program, with `SYNCHRONIZE` access.
    type Holder<'a> = BorrowedHandle<'a>;

    /// Creates `Local\mirrord-debug-release-<pid>-<uuid>`.
    ///
    /// `Local\` keeps it in the user's session, where the IDE that sets it runs. It carries a low
    /// integrity label, so an IDE running at a lower integrity level than `mirrord` can still set
    /// it.
    fn create(pid: u32) -> Result<Self, HoldError> {
        let name = format!(
            r"Local\mirrord-debug-release-{pid}-{}",
            Uuid::new_v4().simple()
        );
        NamedEvent::create_fresh(name, EventAccess::LowIntegrity)
            .map(|event| Self { event, pid })
            .map_err(|source| HoldError::Create { pid, source })
    }

    fn release_name(&self) -> &str {
        self.event.name()
    }

    /// A release that races the program's exit counts as a release, and the resume then finds the
    /// process gone.
    fn wait(&self, program: BorrowedHandle<'_>) -> Result<HoldOutcome, HoldError> {
        loop {
            match wait_any(&[&self.event], program, None) {
                Ok(Wake::Event(_)) => return Ok(HoldOutcome::Released),
                Ok(Wake::ProcessExited) => return Ok(HoldOutcome::ProcessExited),
                // Nothing set a timeout, so there is nothing to give up on.
                Ok(Wake::TimedOut) => continue,
                Err(source) => {
                    return Err(HoldError::Wait {
                        pid: self.pid,
                        source,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::CString,
        os::windows::io::{AsHandle, FromRawHandle, OwnedHandle},
        process::{Command, Stdio},
    };

    use winapi::{
        shared::minwindef::FALSE,
        um::{
            handleapi::CloseHandle,
            processthreadsapi::OpenProcess,
            synchapi::{OpenEventA, SetEvent},
            winnt::{EVENT_MODIFY_STATE, SYNCHRONIZE},
        },
    };

    use super::*;

    /// A handle to this test process, which stays alive, so only a release can end a wait on it.
    fn this_process() -> OwnedHandle {
        let process = unsafe { OpenProcess(SYNCHRONIZE, FALSE, std::process::id()) };
        assert!(!process.is_null(), "open this process");
        // SAFETY: a fresh handle this function owns.
        unsafe { OwnedHandle::from_raw_handle(process.cast()) }
    }

    /// Sets the event by its announced name, with only the right an IDE asks for.
    fn release_like_an_ide(name: &str) {
        let name = CString::new(name).expect("name");
        let event = unsafe { OpenEventA(EVENT_MODIFY_STATE, FALSE, name.as_ptr()) };
        assert!(!event.is_null(), "the IDE opens the announced name");
        unsafe {
            assert_ne!(SetEvent(event), 0, "the IDE sets it");
            CloseHandle(event);
        }
    }

    /// The IDE releases from its own process, so the set comes from another thread here, by name.
    #[test]
    fn a_release_from_another_thread_ends_the_hold() {
        let gate = Gate::create(std::process::id()).expect("create the gate");
        let process = this_process();
        let name = gate.release_name().to_owned();
        let ide = std::thread::spawn(move || release_like_an_ide(&name));

        let outcome = gate.wait(process.as_handle()).expect("wait");
        ide.join().expect("IDE");

        assert_eq!(
            outcome,
            HoldOutcome::Released,
            "the program, still alive, was released"
        );
    }

    /// A held program the IDE stops, or that dies, ends the hold: nobody will release it.
    #[test]
    fn a_program_that_exits_ends_the_hold() {
        let gate = Gate::create(std::process::id()).expect("create the gate");
        // Named with its extension, so there is no shim to resolve.
        #[allow(clippy::disallowed_methods)]
        let mut program = Command::new("cmd.exe")
            .args(["/c", "exit 3"])
            .stdout(Stdio::null())
            .spawn()
            .expect("start a short program");

        let outcome = gate.wait(program.as_handle()).expect("wait");
        program.wait().expect("reap the program");

        assert_eq!(
            outcome,
            HoldOutcome::ProcessExited,
            "nothing released it, so only its exit ended the hold"
        );
    }

    /// An event that already exists may already be set, so a gate under its name could release a
    /// program before anyone attached.
    #[test]
    fn an_existing_release_name_is_refused() {
        let gate = Gate::create(std::process::id()).expect("create the gate");

        let again =
            NamedEvent::create_fresh(gate.release_name().to_owned(), EventAccess::LowIntegrity);

        assert!(
            matches!(again, Err(GateError::Exists { .. })),
            "the name is taken, so it is refused rather than reused"
        );
    }

    /// The same pid twice, as when Windows reuses it, must give two names, so an IDE holding the
    /// first can never release the second.
    #[test]
    fn two_gates_for_one_pid_have_different_names() {
        let pid = std::process::id();
        let first = Gate::create(pid).expect("create the first gate");
        let second = Gate::create(pid).expect("create the second gate");

        let prefix = format!(r"Local\mirrord-debug-release-{pid}-");
        for name in [first.release_name(), second.release_name()] {
            assert!(name.starts_with(&prefix), "named after the pid: {name}");
            assert!(
                !name.contains(char::is_whitespace),
                "the IDE matches release=\\S+: {name}"
            );
        }
        assert_ne!(
            first.release_name(),
            second.release_name(),
            "each launch gets its own name"
        );
    }
}
