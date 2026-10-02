use std::{
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use mirrord_command::resolve_command;

use crate::relative_to_root;

pub fn workspace_command() -> Command {
    let mut command = resolve_command("pnpm");
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

    command_succeeds_with_timeout(resolve_command("pnpm").arg("--version"), PROBE_TIMEOUT)
}

fn corepack_available() -> bool {
    command_succeeds_with_timeout(resolve_command("corepack").arg("--version"), PROBE_TIMEOUT)
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
