use std::{
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::relative_to_root;

pub fn workspace_command() -> Command {
    let mut command = Command::new(command_name());
    command.current_dir(relative_to_root(Path::new(".")));
    command
}

/// Generous because the first `pnpm --version` under a freshly enabled corepack downloads and
/// extracts pnpm itself, which took over 10 seconds on the Windows release runner and made the
/// probe report pnpm as missing (killing the child mid-download).
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

pub fn available_with_corepack_warning() -> bool {
    if !corepack_available() {
        eprintln!(
            "[WARNING] - `corepack` not found in PATH: this may cause builds to fail if a compatible version of `pnpm` is not available"
        )
    }

    command_succeeds_with_timeout(Command::new(command_name()).arg("--version"), PROBE_TIMEOUT)
}

fn corepack_available() -> bool {
    command_succeeds_with_timeout(
        Command::new(corepack_command_name()).arg("--version"),
        PROBE_TIMEOUT,
    )
}

fn command_succeeds_with_timeout(command: &mut Command, timeout: Duration) -> bool {
    let mut child = match command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started_at.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Err(_) => return false,
        }
    }
}

/// Pnpm is installed via `corepack` as a batch script on windows, so `Command::new("pnpm")`
/// fails - [`std::process::Command`] on windows doesn't apply `PATHEXT` (see
/// <https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/start>)
/// and only looks for `pnpm.exe`. Use the `.cmd` shim explicitly there.
fn command_name() -> &'static str {
    if cfg!(windows) { "pnpm.cmd" } else { "pnpm" }
}

/// Corepack itself ships as a batch script on windows (node installs `corepack.cmd`), so the
/// probe needs the same `.cmd` treatment as [`command_name`] or it always reports corepack as
/// missing there.
fn corepack_command_name() -> &'static str {
    if cfg!(windows) {
        "corepack.cmd"
    } else {
        "corepack"
    }
}
