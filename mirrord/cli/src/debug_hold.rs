//! `mirrord exec`'s part of the debug hold: reading `mirrord up`'s request, deciding what holds
//! the program, and what the program gets of it. How the hold travels is in
//! [`mirrord_config::debug_hold`].
//!
//! Two triggers can hold a debugged program, and `exec` picks one per program, from its image
//! ([`decide`]):
//! - **The .NET runtime's startup pause**, for a .NET (Core / 5+) apphost or the `dotnet` muxer.
//!   The runtime pauses on the diagnostic port `mirrord up` serves, and `up` announces it. `exec`
//!   only hands the port on, and holds nothing.
//! - **A process hold**, for anything else: `exec` holds the program before its entry point.
//!
//! Either way the port goes into the program's `DOTNET_DiagnosticPorts`, so the .NET programs it
//! starts pause too.
//!
//! Who holds a process differs by platform, so what the program gets does too:
//! - Windows: `exec` creates the program suspended and stays its parent, so it holds the program
//!   itself, from outside, in the launch. The program gets nothing.
//! - Linux and macOS: `exec` replaces itself with the program through `execve`, so the program's
//!   own layer holds. The program gets `MIRRORD_LAYER_DEBUG_HOLD` set to `exec`'s pid.

use std::path::Path;

use dotnot::DotnetKind;
use mirrord_config::debug_hold::{
    self, MIRRORD_DEBUG_DOTNET_PORT, MIRRORD_DEBUG_HOLD, MIRRORD_LAYER_DEBUG_HOLD,
    dotnet_ports::{self, COMPLUS_DIAGNOSTIC_PORTS, DOTNET_DIAGNOSTIC_PORTS},
};
use mirrord_debug_hold::{DotnetImage, Plan, Trigger};

use crate::process_env::{self, ProcessEnv};

/// What `mirrord up` asked of this `mirrord exec` about the program it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExecHold {
    /// Hold the program before its own code runs, until an IDE releases it: `mirrord up --debug`
    /// named this service.
    Requested {
        /// The diagnostic port `mirrord up` serves for the service, if it serves one.
        dotnet_port: Option<String>,
    },
    /// Run the program as usual.
    NotRequested,
}

impl ExecHold {
    /// Reads `mirrord up`'s request from this process's environment.
    pub(crate) fn from_env() -> Self {
        if debug_hold::requested_in_env() {
            Self::Requested {
                dotnet_port: std::env::var(MIRRORD_DEBUG_DOTNET_PORT)
                    .ok()
                    .filter(|port| !port.is_empty()),
            }
        } else {
            Self::NotRequested
        }
    }

    /// Whether `mirrord up` asked for a hold.
    #[cfg(not(target_os = "windows"))]
    pub(crate) fn is_requested(&self) -> bool {
        matches!(self, Self::Requested { .. })
    }

    /// Decides what holds `root`, the program's image, with `env` the environment it will get.
    /// `None` when no hold was asked for.
    pub(crate) fn plan(&self, root: &Path, env: &ProcessEnv) -> Option<Plan> {
        let Self::Requested { dotnet_port } = self else {
            return None;
        };
        let kind = dotnot::classify(root).map_err(|error| error.to_string());
        let (trigger, dotnet, note) = decide(kind, dotnet_port.is_some(), diagnostics_off(env));
        Some(Plan::new(
            trigger,
            dotnet,
            Some(root.display().to_string()),
            note,
        ))
    }

    /// Makes `env`, the program's environment, carry exactly what the program gets of the hold,
    /// whatever it inherited.
    ///
    /// # Arguments
    ///
    /// * `plan` - what holds the program, from [`Self::plan`].
    /// * `env` - the program's environment, once everything else composed it.
    pub(crate) fn hand_to_program(&self, plan: Option<&Plan>, env: &mut ProcessEnv) {
        // The request is for this `exec` alone: a `mirrord exec` the program starts reads it too,
        // and would hold with no IDE waiting for it.
        env.remove(MIRRORD_DEBUG_HOLD);
        env.remove(MIRRORD_DEBUG_DOTNET_PORT);

        match self {
            Self::Requested {
                dotnet_port: Some(port),
            } => {
                let merged = dotnet_ports::merge(
                    process_env::get(env, DOTNET_DIAGNOSTIC_PORTS),
                    process_env::get(env, COMPLUS_DIAGNOSTIC_PORTS),
                    port,
                );
                process_env::set(env, DOTNET_DIAGNOSTIC_PORTS, merged);
            }
            // An entry of an outer `mirrord up` names a port nobody may serve any more, which
            // would pause a runtime forever.
            Self::Requested { dotnet_port: None } | Self::NotRequested => {
                for name in [DOTNET_DIAGNOSTIC_PORTS, COMPLUS_DIAGNOSTIC_PORTS] {
                    let kept =
                        process_env::get(env, name).map(dotnet_ports::without_mirrord_entries);
                    match kept {
                        Some(Some(kept)) => process_env::set(env, name, kept),
                        Some(None) => {
                            env.remove(name);
                        }
                        None => {}
                    }
                }
            }
        }

        let holds_process = plan.is_some_and(|plan| plan.trigger == Trigger::Process);
        match platform::layer_pid(holds_process) {
            Some(pid) => process_env::set(env, MIRRORD_LAYER_DEBUG_HOLD, pid.to_string()),
            None => {
                env.remove(MIRRORD_LAYER_DEBUG_HOLD);
            }
        }
    }
}

/// The trigger for a root of `kind`, the kind of .NET program it is (`dotnot`'s answer, or why it
/// couldn't tell), with the note `debug-plan` carries when the trigger isn't the one the kind
/// asks for.
///
/// Only a CoreCLR host pauses at its runtime's startup, and only when there is a port to pause on
/// and its diagnostics are on. Everything else gets a process hold, which works for any program.
///
/// # Arguments
///
/// * `kind` - the root's kind, or the error that kept `dotnot` from reading it.
/// * `has_port` - whether `mirrord up` serves a diagnostic port for the service.
/// * `diagnostics_off` - whether the program's environment turns the runtime's diagnostics off.
fn decide(
    kind: Result<DotnetKind, String>,
    has_port: bool,
    diagnostics_off: bool,
) -> (Trigger, Option<DotnetImage>, Option<String>) {
    let image = match kind {
        Ok(DotnetKind::Apphost { single_file: false }) => DotnetImage::Apphost,
        Ok(DotnetKind::Apphost { single_file: true }) => DotnetImage::SingleFile,
        Ok(DotnetKind::Muxer) => DotnetImage::Muxer,
        Ok(DotnetKind::FrameworkAssembly) => {
            return (Trigger::Process, Some(DotnetImage::Framework), None);
        }
        Ok(DotnetKind::NotDotnet) => return (Trigger::Process, None, None),
        Err(error) => {
            return (
                Trigger::Process,
                None,
                Some(format!(
                    "couldn't tell whether the program is .NET ({error}), so it's held at its start"
                )),
            );
        }
    };

    let note = if !has_port {
        "mirrord up serves no diagnostic port for this service, so the .NET program is held at \
         its start"
    } else if diagnostics_off {
        "DOTNET_EnableDiagnostics turns the runtime's diagnostics off, so it can't pause, and the \
         .NET program is held at its start"
    } else {
        return (Trigger::Runtime, Some(image), None);
    };
    (Trigger::Process, Some(image), Some(note.to_owned()))
}

/// Whether `env` turns off the diagnostic server a runtime pauses on.
///
/// The runtime starts it unless `EnableDiagnostics` or `EnableDiagnostics_IPC` is 0, each read as
/// `DOTNET_<name>`, else `COMPlus_<name>`, and parsed as hex like every runtime setting.
fn diagnostics_off(env: &ProcessEnv) -> bool {
    ["EnableDiagnostics", "EnableDiagnostics_IPC"]
        .iter()
        .any(|name| {
            let value = process_env::get(env, &format!("DOTNET_{name}"))
                .or_else(|| process_env::get(env, &format!("COMPlus_{name}")));
            value.is_some_and(|value| u32::from_str_radix(value.trim(), 16) == Ok(0))
        })
}

#[cfg(not(target_os = "windows"))]
mod platform {
    /// The pid whose layer holds the program: this process's own, when the program is held at
    /// its start.
    ///
    /// TRICK: the `execve` that starts the program replaces this process's image with the
    /// program's, but keeps its pid, its stdio pipes and the environment it's handed. So this pid
    /// becomes the program's pid, and the layer holds only the process it names: the program
    /// holds, and a child that inherits the variable never does.
    pub(super) fn layer_pid(holds_process: bool) -> Option<u32> {
        holds_process.then(std::process::id)
    }
}

#[cfg(target_os = "windows")]
mod platform {
    /// None: `exec` holds the program itself, in the launch, so no layer holds it.
    pub(super) fn layer_pid(_holds_process: bool) -> Option<u32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// The hold variables the program gets.
    #[derive(Debug, PartialEq)]
    struct ProgramGets {
        request: Option<String>,
        port_request: Option<String>,
        layer_pid: Option<String>,
        ports: Option<String>,
    }

    const PORT: &str = "mirrord-dotnet-ab12";

    /// What the program gets of the hold, when `exec` decided `hold` and `trigger`.
    fn program_gets(hold: ExecHold, trigger: Option<Trigger>) -> ProgramGets {
        // The worst start: the program's environment still carries `mirrord up`'s request, its
        // port, a stale pid for a layer, and a stale port of an outer `mirrord up`, as `exec`
        // inherited them.
        let mut env = process_env::from_entries([
            (MIRRORD_DEBUG_HOLD, debug_hold::REQUESTED.to_owned()),
            (MIRRORD_DEBUG_DOTNET_PORT, PORT.to_owned()),
            (MIRRORD_LAYER_DEBUG_HOLD, "4242".to_owned()),
            (
                DOTNET_DIAGNOSTIC_PORTS,
                "mirrord-dotnet-old,connect,suspend;my-tool".to_owned(),
            ),
        ]);
        let plan = trigger.map(|trigger| Plan::new(trigger, None, None, None));

        hold.hand_to_program(plan.as_ref(), &mut env);

        // Read back as the program's layer, its runtime, or a `mirrord exec` it starts would.
        let read = |name| process_env::get(&env, name).map(str::to_owned);
        ProgramGets {
            request: read(MIRRORD_DEBUG_HOLD),
            port_request: read(MIRRORD_DEBUG_DOTNET_PORT),
            layer_pid: read(MIRRORD_LAYER_DEBUG_HOLD),
            ports: read(DOTNET_DIAGNOSTIC_PORTS),
        }
    }

    fn requested_with_port() -> ExecHold {
        ExecHold::Requested {
            dotnet_port: Some(PORT.to_owned()),
        }
    }

    /// On Linux and macOS a process hold is the program's layer's, so the program gets this
    /// process's pid, which `execve` hands to it, and never the request.
    #[cfg(not(windows))]
    #[test]
    fn a_process_hold_is_handed_to_the_programs_layer() {
        assert_eq!(
            program_gets(requested_with_port(), Some(Trigger::Process)),
            ProgramGets {
                request: None,
                port_request: None,
                layer_pid: Some(std::process::id().to_string()),
                ports: Some(format!("my-tool;{PORT},connect,suspend")),
            }
        );
    }

    /// On Windows `exec` holds the program itself, so its layer is never asked to. The .NET
    /// programs it starts still get the port.
    #[cfg(windows)]
    #[test]
    fn a_process_hold_stays_with_exec() {
        assert_eq!(
            program_gets(requested_with_port(), Some(Trigger::Process)),
            ProgramGets {
                request: None,
                port_request: None,
                layer_pid: None,
                ports: Some(format!("my-tool;{PORT},connect,suspend")),
            }
        );
    }

    /// The runtime holds a .NET root at its own pause, so no layer may hold it before that: the
    /// IDE would get two holds for one process.
    #[test]
    fn a_runtime_hold_asks_no_layer_to_hold() {
        assert_eq!(
            program_gets(requested_with_port(), Some(Trigger::Runtime)),
            ProgramGets {
                request: None,
                port_request: None,
                layer_pid: None,
                ports: Some(format!("my-tool;{PORT},connect,suspend")),
            },
            "the runtime gets the port, and nothing else of the hold"
        );
    }

    /// Without a request, whatever was inherited is dropped, on every platform, and a stale port
    /// of an outer `mirrord up` too, since nobody may serve it any more.
    #[test]
    fn without_a_request_the_program_gets_nothing() {
        assert_eq!(
            program_gets(ExecHold::NotRequested, None),
            ProgramGets {
                request: None,
                port_request: None,
                layer_pid: None,
                ports: Some("my-tool".to_owned()),
            }
        );
    }

    /// The trigger for each kind of root. Only a CoreCLR host with a port to pause on, and its
    /// diagnostics on, waits for its runtime; everything else is held at its start, and a .NET
    /// host held at its start says why in its plan.
    #[rstest]
    #[case::apphost(
        Ok(DotnetKind::Apphost { single_file: false }),
        true,
        false,
        (Trigger::Runtime, Some(DotnetImage::Apphost)),
        false
    )]
    #[case::single_file(
        Ok(DotnetKind::Apphost { single_file: true }),
        true,
        false,
        (Trigger::Runtime, Some(DotnetImage::SingleFile)),
        false
    )]
    #[case::muxer(
        Ok(DotnetKind::Muxer),
        true,
        false,
        (Trigger::Runtime, Some(DotnetImage::Muxer)),
        false
    )]
    // .NET Framework has no diagnostic port, so its runtime never pauses.
    #[case::framework(
        Ok(DotnetKind::FrameworkAssembly),
        true,
        false,
        (Trigger::Process, Some(DotnetImage::Framework)),
        false
    )]
    #[case::native(Ok(DotnetKind::NotDotnet), true, false, (Trigger::Process, None), false)]
    // The odd ones out: a CoreCLR host that can't pause is still held, at its start.
    #[case::diagnostics_off(
        Ok(DotnetKind::Muxer),
        true,
        true,
        (Trigger::Process, Some(DotnetImage::Muxer)),
        true
    )]
    #[case::no_port(
        Ok(DotnetKind::Apphost { single_file: false }),
        false,
        false,
        (Trigger::Process, Some(DotnetImage::Apphost)),
        true
    )]
    #[case::unreadable(Err("denied".to_owned()), true, false, (Trigger::Process, None), true)]
    fn each_root_gets_the_trigger_that_can_hold_it(
        #[case] kind: Result<DotnetKind, String>,
        #[case] has_port: bool,
        #[case] diagnostics_off: bool,
        #[case] expected: (Trigger, Option<DotnetImage>),
        #[case] noted: bool,
    ) {
        let (trigger, dotnet, note) = decide(kind, has_port, diagnostics_off);

        assert_eq!((trigger, dotnet), expected);
        assert_eq!(
            note.is_some(),
            noted,
            "only a plan that differs from what the kind asks for says why: {note:?}"
        );
    }

    /// The runtime reads its settings as hex, the `DOTNET_` spelling before the `COMPlus_` one,
    /// and runs its diagnostic server unless one of the two settings is 0.
    #[rstest]
    #[case::unset(&[], false)]
    #[case::off(&[("DOTNET_EnableDiagnostics", "0")], true)]
    #[case::ipc_off(&[("DOTNET_EnableDiagnostics_IPC", "0")], true)]
    #[case::old_spelling_off(&[("COMPlus_EnableDiagnostics", "0")], true)]
    #[case::on(&[("DOTNET_EnableDiagnostics", "1")], false)]
    // The odd one out: the new spelling wins, so the old one's 0 doesn't count.
    #[case::new_spelling_wins(
        &[("DOTNET_EnableDiagnostics", "1"), ("COMPlus_EnableDiagnostics", "0")],
        false
    )]
    fn diagnostics_are_off_only_when_the_runtime_reads_a_zero(
        #[case] entries: &[(&str, &str)],
        #[case] off: bool,
    ) {
        let env = process_env::from_entries(
            entries
                .iter()
                .map(|(name, value)| (*name, (*value).to_owned())),
        );
        assert_eq!(diagnostics_off(&env), off, "{entries:?}");
    }
}
