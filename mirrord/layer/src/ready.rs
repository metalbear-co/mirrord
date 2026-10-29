//! The end of the layer's startup, which every startup path reaches before the constructor
//! returns and the program's own code runs.
//!
//! `mirrord exec` on Linux and macOS replaces itself with the program before the layer exists, so
//! in the progress modes machines read (`simple` for `mirrord up`, `json` for IDEs) it leaves
//! `Ready!` for the layer to print from here (see [`MIRRORD_LAYER_READY`]). `Ready!` then means
//! what it means on Windows: the layer is in, and has finished starting (connecting to the
//! internal proxy included). In a terminal, `exec` still prints it in its own tree.
//!
//! When `mirrord up --debug` asked for it, the layer then holds the program here for a debugger,
//! before `main` and the executable's own initializers run (see [`MIRRORD_DEBUG_HOLD`]).

use std::{
    ffi::OsStr,
    io::{self, Write},
    ops::Not,
};

use mirrord_config::{MIRRORD_DEBUG_HOLD, MIRRORD_LAYER_READY};
use mirrord_layer_lib::{
    detour::DetourGuard,
    error::LayerError,
    process::hold::{Gate, HoldError, hold},
};
use mirrord_progress::{
    LineMode, finished_task_line,
    messages::{EXEC_RUNNING_TASK, SESSION_READY_MESSAGE},
};

use crate::program_stdout::ProgramStdout;

/// Finishes the layer's startup: prints `Ready!` if `mirrord exec` left it to the layer, then
/// holds the program for a debugger if `exec` asked for that.
///
/// The order matches Windows: `Ready!`, then the hold's announcement, then the wait.
pub(crate) fn layer_ready() -> Result<(), LayerError> {
    // What the layer does here is its own, so none of it goes through the hooks.
    let _detour_guard = DetourGuard::new();

    if let Some(request) = std::env::var_os(MIRRORD_LAYER_READY) {
        // Only this process reports `Ready!`, so its children must never get the variable. The
        // macOS env backup in `mirrord_layer_lib::setup` leaves it out too.
        // SAFETY: the constructor runs before the program's own threads start, so environment
        // mutation cannot race another thread.
        unsafe { std::env::remove_var(MIRRORD_LAYER_READY) };

        let pid = std::process::id();
        match parse_ready_request(&request) {
            Some((mode, bound_to)) if bound_to == pid => {
                if let Err(error) = print_ready(mode) {
                    tracing::warn!(%error, "failed to print Ready!");
                }
            }
            // A child an earlier library constructor started inherits the variable, under
            // another pid.
            Some((_, bound_to)) => {
                tracing::debug!(bound_to, pid, "Ready! is bound to another process");
            }
            None => tracing::warn!(?request, "ignoring a malformed {MIRRORD_LAYER_READY}"),
        }
    }

    debug_hold().map_err(LayerError::from)
}

/// Holds the program until an IDE releases it, when `mirrord exec` bound [`MIRRORD_DEBUG_HOLD`]
/// to this process.
///
/// `mirrord exec` sets the variable to its own pid, then replaces itself with the program through
/// `execve`, which keeps the pid. So the value names the program, and a process that still
/// inherits it, such as a child an earlier library constructor started, has another pid and runs
/// on.
fn debug_hold() -> Result<(), HoldError> {
    let Some(bound_to) = std::env::var_os(MIRRORD_DEBUG_HOLD) else {
        return Ok(());
    };
    // Only this process is held, so its children must never get the variable. The macOS env
    // backup in `mirrord_layer_lib::setup` leaves it out too.
    // SAFETY: the constructor runs before the program's own threads start, so environment
    // mutation cannot race another thread.
    unsafe { std::env::remove_var(MIRRORD_DEBUG_HOLD) };

    let pid = std::process::id();
    if bound_to.to_str().and_then(|value| value.parse().ok()) != Some(pid) {
        tracing::debug!(?bound_to, pid, "the debug hold is bound to another process");
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    allow_any_tracer();

    // The process that waits is the held one, so the wait can only end in a release.
    hold::<Gate>(pid, (), &mut ProgramStdout)?;
    Ok(())
}

/// Lets any process attach a debugger to this one.
///
/// With Yama's `ptrace_scope=1`, only an ancestor may attach by default, and the IDE's debugger
/// isn't one. `ptrace_scope=2` and `3` still refuse it, as the system demands.
#[cfg(target_os = "linux")]
fn allow_any_tracer() {
    let unused: libc::c_ulong = 0;
    // SAFETY: `PR_SET_PTRACER` reads only its second argument, a pid or `PR_SET_PTRACER_ANY`, and
    // touches no memory.
    let result = unsafe {
        libc::prctl(
            libc::PR_SET_PTRACER,
            libc::PR_SET_PTRACER_ANY,
            unused,
            unused,
            unused,
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        // `EINVAL`: the kernel has no Yama, so nothing restricts the attach.
        if error.raw_os_error() != Some(libc::EINVAL) {
            tracing::warn!(%error, "failed to let a debugger attach; the IDE's attach may be refused");
        }
    }
}

/// Parses [`MIRRORD_LAYER_READY`]'s `<mode>:<pid>`: how to print `Ready!`, and the only process
/// that prints it. Anything else is `None`.
///
/// `mirrord exec` sets the pid to its own, then replaces itself with the program through `execve`,
/// which keeps the pid. So the pid names the program.
///
/// Only the modes machines read: in a terminal's `std` mode, `exec` prints `Ready!` in its own
/// spinner tree before `execve`, and never asks the layer.
fn parse_ready_request(request: &OsStr) -> Option<(LineMode, u32)> {
    let (mode, pid) = request.to_str()?.split_once(':')?;
    let mode = LineMode::from_name(mode)?;
    // Digits only: `u32::from_str` also takes a leading `+`.
    if pid.is_empty() || pid.bytes().all(|byte| byte.is_ascii_digit()).not() {
        return None;
    }
    Some((mode, pid.parse().ok()?))
}

/// Prints `Ready!` on the program's stdout, as `exec`'s progress would have in `mode`.
fn print_ready(mode: LineMode) -> io::Result<()> {
    let line = finished_task_line(mode, EXEC_RUNNING_TASK, SESSION_READY_MESSAGE);
    ProgramStdout.write_all(line.as_bytes())
}
