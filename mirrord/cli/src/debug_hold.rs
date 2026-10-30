//! `mirrord exec`'s part of the debug hold: reading `mirrord up`'s request, and deciding what the
//! program it starts gets of it. How the hold travels is in [`mirrord_config::debug_hold`].
//!
//! Who holds the program differs by platform, so what the program gets does too:
//! - Windows: `exec` creates the program suspended and stays its parent, so it holds the program
//!   itself, from outside, in the launch. The program gets nothing.
//! - Linux and macOS: `exec` replaces itself with the program through `execve`, so the program's
//!   own layer holds. The program gets `MIRRORD_LAYER_DEBUG_HOLD` set to `exec`'s pid.

use mirrord_config::debug_hold::{self, MIRRORD_DEBUG_HOLD, MIRRORD_LAYER_DEBUG_HOLD};

use crate::process_env::{self, ProcessEnv};

/// What `mirrord up` asked of this `mirrord exec` about the program it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecHold {
    /// Hold the program before its own code runs, until an IDE releases it: `mirrord up --debug`
    /// named this service.
    Requested,
    /// Run the program as usual.
    NotRequested,
}

impl ExecHold {
    /// Reads `mirrord up`'s request from this process's environment.
    pub(crate) fn from_env() -> Self {
        if debug_hold::requested_in_env() {
            Self::Requested
        } else {
            Self::NotRequested
        }
    }

    /// Makes `env`, the program's environment, carry exactly what the program gets of the hold,
    /// whatever it inherited.
    pub(crate) fn hand_to_program(self, env: &mut ProcessEnv) {
        // The request is for this `exec` alone: a `mirrord exec` the program starts reads it too,
        // and would hold with no IDE waiting for it.
        env.remove(MIRRORD_DEBUG_HOLD);

        match platform::layer_pid(self) {
            Some(pid) => process_env::set(env, MIRRORD_LAYER_DEBUG_HOLD, pid.to_string()),
            None => {
                env.remove(MIRRORD_LAYER_DEBUG_HOLD);
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::ExecHold;

    /// The pid whose layer holds the program: this process's own.
    ///
    /// TRICK: the `execve` that starts the program replaces this process's image with the
    /// program's, but keeps its pid, its stdio pipes and the environment it's handed. So this pid
    /// becomes the program's pid, and the layer holds only the process it names: the program
    /// holds, and a child that inherits the variable never does.
    pub(super) fn layer_pid(hold: ExecHold) -> Option<u32> {
        match hold {
            ExecHold::Requested => Some(std::process::id()),
            ExecHold::NotRequested => None,
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::ExecHold;

    /// None: `exec` holds the program itself, in the launch, so no layer holds it.
    pub(super) fn layer_pid(_hold: ExecHold) -> Option<u32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hold variables the program gets.
    #[derive(Debug, PartialEq)]
    struct ProgramGets {
        request: Option<String>,
        layer_pid: Option<String>,
    }

    /// What the program gets of the hold, when `exec` decided `hold`.
    fn program_gets(hold: ExecHold) -> ProgramGets {
        // The worst start: the program's environment still carries `mirrord up`'s request, and a
        // stale pid for a layer, as `exec` inherited them.
        let mut env = process_env::from_entries([
            (MIRRORD_DEBUG_HOLD, debug_hold::REQUESTED.to_owned()),
            (MIRRORD_LAYER_DEBUG_HOLD, "4242".to_owned()),
        ]);

        hold.hand_to_program(&mut env);

        // Read back as the program's layer, or a `mirrord exec` it starts, would read them.
        let read = |name| process_env::get(&env, name).map(str::to_owned);
        ProgramGets {
            request: read(MIRRORD_DEBUG_HOLD),
            layer_pid: read(MIRRORD_LAYER_DEBUG_HOLD),
        }
    }

    /// On Linux and macOS the program's layer holds, so the program gets this process's pid,
    /// which `execve` hands to it, and never the request.
    #[cfg(not(windows))]
    #[test]
    fn a_requested_hold_is_handed_to_the_programs_layer() {
        assert_eq!(
            program_gets(ExecHold::Requested),
            ProgramGets {
                request: None,
                layer_pid: Some(std::process::id().to_string()),
            }
        );
    }

    /// On Windows `exec` holds the program itself, so the program gets nothing.
    #[cfg(windows)]
    #[test]
    fn a_requested_hold_stays_with_exec() {
        assert_eq!(
            program_gets(ExecHold::Requested),
            ProgramGets {
                request: None,
                layer_pid: None,
            }
        );
    }

    /// Without a request, whatever was inherited is dropped, on every platform.
    #[test]
    fn without_a_request_the_program_gets_nothing() {
        assert_eq!(
            program_gets(ExecHold::NotRequested),
            ProgramGets {
                request: None,
                layer_pid: None,
            }
        );
    }
}
