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
use mirrord_debug_hold::{DotnetImage, Fallback, Plan, Trigger};

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
        let (trigger, dotnet, fallback) =
            decide(kind, dotnet_port.is_some(), &Diagnostics::of(env));
        let (fallback, note) = fallback.unzip();
        Some(Plan::new(
            trigger,
            dotnet,
            Some(root.display().to_string()),
            fallback,
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
/// couldn't tell), with the fallback and note `debug-plan` carries when the trigger isn't the one
/// the kind asks for.
///
/// Only a CoreCLR host pauses at its runtime's startup, and only when there is a port to pause on
/// and its diagnostic server is on. Everything else gets a process hold, which works for any
/// program. A CoreCLR host whose debugger is off is held too, and the plan says no debugger can
/// attach, so the IDE releases it without one.
///
/// # Arguments
///
/// * `kind` - the root's kind, or the error that kept `dotnot` from reading it.
/// * `has_port` - whether `mirrord up` serves a diagnostic port for the service.
/// * `diagnostics` - what the program's environment leaves on of the runtime's diagnostics.
fn decide(
    kind: Result<DotnetKind, String>,
    has_port: bool,
    diagnostics: &Diagnostics,
) -> (Trigger, Option<DotnetImage>, Option<(Fallback, String)>) {
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
                Some((
                    Fallback::UnknownKind,
                    format!(
                        "couldn't tell whether the program is .NET ({error}), so it's held at its \
                         start"
                    ),
                )),
            );
        }
    };

    // A debugger that can't attach decides first: a port or a pause changes nothing for it.
    let (fallback, note) = match diagnostics {
        Diagnostics::DebuggerOff { setting } => (
            Fallback::DiagnosticsOff,
            format!(
                "{setting}=0 turns the runtime's debugger off, so no debugger can attach to the \
                 .NET program; it's held at its start, for the IDE to release"
            ),
        ),
        _ if !has_port => (
            Fallback::NoPort,
            "mirrord up serves no diagnostic port for this service, so the .NET program is held \
             at its start"
                .to_owned(),
        ),
        Diagnostics::ServerOff { setting } => (
            Fallback::IpcOff,
            format!(
                "{setting}=0 turns off the diagnostic server the runtime pauses on, so the .NET \
                 program is held at its start"
            ),
        ),
        Diagnostics::On => return (Trigger::Runtime, Some(image), None),
    };
    (Trigger::Process, Some(image), Some((fallback, note)))
}

/// What a program's environment leaves on of the .NET runtime's diagnostics, as the runtime reads
/// its settings: `DOTNET_<name>`, else `COMPlus_<name>`, parsed as hex.
///
/// The runtime gates its debugger on `EnableDiagnostics` and `EnableDiagnostics_Debugger`, and its
/// diagnostic server, which a startup pause needs, on `EnableDiagnostics` and
/// `EnableDiagnostics_IPC`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Diagnostics {
    /// The debugger and the diagnostic server are both on.
    On,
    /// Only the diagnostic server is off: the runtime can't pause, but a debugger still attaches.
    ServerOff {
        /// The variable that turned it off, in the spelling the runtime read.
        setting: String,
    },
    /// The debugger is off, so no debugger can attach, whatever holds the program.
    DebuggerOff {
        /// The variable that turned it off, in the spelling the runtime read.
        setting: String,
    },
}

impl Diagnostics {
    /// Reads `env`, the program's environment.
    fn of(env: &ProcessEnv) -> Self {
        // The spelling the runtime reads `name` from, when the value there is 0.
        let zero = |name: &str| {
            let (setting, value) = [format!("DOTNET_{name}"), format!("COMPlus_{name}")]
                .into_iter()
                .find_map(|setting| {
                    process_env::get(env, &setting).map(|value| (setting, value.to_owned()))
                })?;
            (u32::from_str_radix(value.trim(), 16) == Ok(0)).then_some(setting)
        };

        if let Some(setting) =
            zero("EnableDiagnostics").or_else(|| zero("EnableDiagnostics_Debugger"))
        {
            Self::DebuggerOff { setting }
        } else if let Some(setting) = zero("EnableDiagnostics_IPC") {
            Self::ServerOff { setting }
        } else {
            Self::On
        }
    }
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
        let plan = trigger.map(|trigger| Plan::new(trigger, None, None, None, None));

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
    /// diagnostic server on, waits for its runtime; everything else is held at its start, and a
    /// .NET host held at its start says why in its plan, as a fallback an IDE matches.
    #[rstest]
    #[case::apphost(
        Ok(DotnetKind::Apphost { single_file: false }),
        true,
        Diagnostics::On,
        (Trigger::Runtime, Some(DotnetImage::Apphost)),
        None
    )]
    #[case::single_file(
        Ok(DotnetKind::Apphost { single_file: true }),
        true,
        Diagnostics::On,
        (Trigger::Runtime, Some(DotnetImage::SingleFile)),
        None
    )]
    #[case::muxer(
        Ok(DotnetKind::Muxer),
        true,
        Diagnostics::On,
        (Trigger::Runtime, Some(DotnetImage::Muxer)),
        None
    )]
    // .NET Framework has no diagnostic port, so its runtime never pauses.
    #[case::framework(
        Ok(DotnetKind::FrameworkAssembly),
        true,
        Diagnostics::On,
        (Trigger::Process, Some(DotnetImage::Framework)),
        None
    )]
    #[case::native(
        Ok(DotnetKind::NotDotnet),
        true,
        Diagnostics::On,
        (Trigger::Process, None),
        None
    )]
    // The odd ones out: a CoreCLR host that can't pause is still held, at its start.
    // Its debugger is off, so the plan says no debugger can attach.
    #[case::debugger_off(
        Ok(DotnetKind::Muxer),
        true,
        Diagnostics::DebuggerOff { setting: "DOTNET_EnableDiagnostics_Debugger".to_owned() },
        (Trigger::Process, Some(DotnetImage::Muxer)),
        Some(Fallback::DiagnosticsOff)
    )]
    // No port to pause on changes nothing for a debugger that can't attach.
    #[case::debugger_off_without_a_port(
        Ok(DotnetKind::Muxer),
        false,
        Diagnostics::DebuggerOff { setting: "DOTNET_EnableDiagnostics".to_owned() },
        (Trigger::Process, Some(DotnetImage::Muxer)),
        Some(Fallback::DiagnosticsOff)
    )]
    // Only its diagnostic server is off: it can't pause, but a debugger attaches at its start.
    #[case::server_off(
        Ok(DotnetKind::Apphost { single_file: false }),
        true,
        Diagnostics::ServerOff { setting: "DOTNET_EnableDiagnostics_IPC".to_owned() },
        (Trigger::Process, Some(DotnetImage::Apphost)),
        Some(Fallback::IpcOff)
    )]
    #[case::no_port(
        Ok(DotnetKind::Apphost { single_file: false }),
        false,
        Diagnostics::On,
        (Trigger::Process, Some(DotnetImage::Apphost)),
        Some(Fallback::NoPort)
    )]
    #[case::unreadable(
        Err("denied".to_owned()),
        true,
        Diagnostics::On,
        (Trigger::Process, None),
        Some(Fallback::UnknownKind)
    )]
    fn each_root_gets_the_trigger_that_can_hold_it(
        #[case] kind: Result<DotnetKind, String>,
        #[case] has_port: bool,
        #[case] diagnostics: Diagnostics,
        #[case] expected: (Trigger, Option<DotnetImage>),
        #[case] expected_fallback: Option<Fallback>,
    ) {
        let (trigger, dotnet, fallback) = decide(kind, has_port, &diagnostics);

        assert_eq!((trigger, dotnet), expected);
        assert_eq!(
            fallback.as_ref().map(|(fallback, _)| *fallback),
            expected_fallback,
            "only a plan that differs from what the kind asks for says why: {fallback:?}"
        );
    }

    /// The runtime reads its settings as hex, the `DOTNET_` spelling before the `COMPlus_` one.
    /// `EnableDiagnostics` and `EnableDiagnostics_Debugger` turn its debugger off, and
    /// `EnableDiagnostics_IPC` only its diagnostic server.
    #[rstest]
    #[case::unset(&[], Diagnostics::On)]
    #[case::on(&[("DOTNET_EnableDiagnostics", "1")], Diagnostics::On)]
    #[case::all_off(
        &[("DOTNET_EnableDiagnostics", "0")],
        Diagnostics::DebuggerOff { setting: "DOTNET_EnableDiagnostics".to_owned() }
    )]
    #[case::debugger_off(
        &[("DOTNET_EnableDiagnostics_Debugger", "0")],
        Diagnostics::DebuggerOff { setting: "DOTNET_EnableDiagnostics_Debugger".to_owned() }
    )]
    #[case::old_spelling_debugger_off(
        &[("COMPlus_EnableDiagnostics_Debugger", "0")],
        Diagnostics::DebuggerOff { setting: "COMPlus_EnableDiagnostics_Debugger".to_owned() }
    )]
    #[case::server_off(
        &[("DOTNET_EnableDiagnostics_IPC", "0")],
        Diagnostics::ServerOff { setting: "DOTNET_EnableDiagnostics_IPC".to_owned() }
    )]
    #[case::old_spelling_off(
        &[("COMPlus_EnableDiagnostics", "0")],
        Diagnostics::DebuggerOff { setting: "COMPlus_EnableDiagnostics".to_owned() }
    )]
    // A debugger that can't attach outweighs a server that's off too.
    #[case::debugger_and_server_off(
        &[("DOTNET_EnableDiagnostics_IPC", "0"), ("DOTNET_EnableDiagnostics_Debugger", "0")],
        Diagnostics::DebuggerOff { setting: "DOTNET_EnableDiagnostics_Debugger".to_owned() }
    )]
    // The odd one out: the new spelling wins, so the old one's 0 doesn't count.
    #[case::new_spelling_wins(
        &[("DOTNET_EnableDiagnostics", "1"), ("COMPlus_EnableDiagnostics", "0")],
        Diagnostics::On
    )]
    fn the_runtime_reads_which_diagnostics_are_off(
        #[case] entries: &[(&str, &str)],
        #[case] expected: Diagnostics,
    ) {
        let env = process_env::from_entries(
            entries
                .iter()
                .map(|(name, value)| (*name, (*value).to_owned())),
        );
        assert_eq!(Diagnostics::of(&env), expected, "{entries:?}");
    }
}
