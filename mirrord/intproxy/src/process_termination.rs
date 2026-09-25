//! Termination of the processes mirrord is injected into.
//!
//! The intproxy is the only mirrord-controlled process that survives for the whole session
//! (`mirrord exec` replaces the CLI with the user binary via `execv`), so it is also the only place
//! that can tear the injected processes down when the session cannot continue. The layers
//! themselves only notice a broken session lazily, on their next hooked syscall, so a process idle
//! in `accept()` would otherwise linger forever holding its ports.
//!
//! The termination itself is platform specific and is shared by every shutdown path in the
//! intproxy, so it lives here instead of being duplicated per caller.

use std::collections::HashSet;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{Signal, kill},
    unistd::Pid,
};
#[cfg(unix)]
use tokio::time;
#[cfg(windows)]
use winapi::{
    shared::minwindef::FALSE,
    um::{
        errhandlingapi::GetLastError,
        handleapi::CloseHandle,
        processthreadsapi::{OpenProcess, TerminateProcess},
        winnt::PROCESS_TERMINATE,
    },
};

/// Grace period between the `SIGTERM` and the `SIGKILL` we send to the injected processes, giving
/// them a chance to run their own shutdown before we force the issue.
/// Shortened under test so tests can exercise the real termination path without a slow wait.
#[cfg(all(unix, not(test)))]
const TERMINATION_GRACE: Duration = Duration::from_secs(2);
#[cfg(all(unix, test))]
const TERMINATION_GRACE: Duration = Duration::from_millis(50);

/// On unix, sends `SIGTERM` to the given processes, then `SIGKILL` to the same set after
/// [`TERMINATION_GRACE`], so well-behaved processes get to run their shutdown first.
///
/// A process that already exited is the outcome we want, so the resulting `ESRCH` is success. Any
/// other signalling error is logged and the remaining processes are still signalled.
#[cfg(unix)]
pub(crate) async fn terminate_processes(pids: HashSet<i32>) {
    terminate_processes_with(pids, send_signal).await;
}

#[cfg(unix)]
async fn terminate_processes_with(pids: HashSet<i32>, mut signal_process: impl FnMut(i32, Signal)) {
    if pids.is_empty() {
        return;
    }

    for pid in &pids {
        signal_process(*pid, Signal::SIGTERM);
    }

    time::sleep(TERMINATION_GRACE).await;

    for pid in pids {
        signal_process(pid, Signal::SIGKILL);
    }
}

#[cfg(unix)]
fn send_signal(pid: i32, signal: Signal) {
    match kill(Pid::from_raw(pid), signal) {
        // `ESRCH` just means the process already exited, which is the outcome we want.
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(error) => tracing::warn!(
            pid,
            ?signal,
            %error,
            "Failed to signal an injected process while tearing down a session",
        ),
    }
}

/// On Windows, calls `TerminateProcess` on each pid. No reliable graceful signal exists for an
/// arbitrary process here, so this matches the unix `SIGKILL` with no grace phase.
#[cfg(windows)]
pub(crate) async fn terminate_processes(pids: HashSet<i32>) {
    if pids.is_empty() {
        return;
    }

    for pid in pids {
        // SAFETY: FFI. Every opened handle is closed. `GetLastError` is read immediately after
        // the failing call, before anything else can clobber the thread-local error.
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, FALSE, pid as u32);
            if handle.is_null() {
                // Most likely the process already exited (the `ESRCH` equivalent), but log the
                // error code so that case can be told apart from a real failure.
                tracing::warn!(
                    pid,
                    error = GetLastError(),
                    "Failed to open an injected process while tearing down a session",
                );
                continue;
            }
            if TerminateProcess(handle, 1) == 0 {
                tracing::warn!(
                    pid,
                    error = GetLastError(),
                    "Failed to terminate an injected process while tearing down a session",
                );
            }
            CloseHandle(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, process::Command, time::Duration};

    use super::terminate_processes;
    #[cfg(unix)]
    use super::terminate_processes_with;

    /// Records the signal phases without signalling arbitrary PIDs on the host.
    #[cfg(unix)]
    async fn terminate_processes_with_signal_recorder(
        pids: HashSet<i32>,
        recorder: &mut Vec<(i32, nix::sys::signal::Signal)>,
    ) {
        terminate_processes_with(pids, |pid, signal| recorder.push((pid, signal))).await;
    }

    /// Spawns a shell script that announces readiness on stdout, and returns only once that
    /// announcement arrived.
    ///
    /// Signal dispositions are only in place after the shell has run its `trap` builtin, so a test
    /// that signals a freshly spawned child would otherwise race the shell's own startup and kill
    /// it with the default disposition.
    #[cfg(unix)]
    fn spawn_ready_script(script: &str) -> std::process::Child {
        use std::{
            io::{BufRead, BufReader},
            process::Stdio,
        };

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();

        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "ready", "test script failed to start");

        child
    }

    /// Spawns a real, long-lived child process for the current platform.
    fn spawn_blocking_child() -> std::process::Child {
        #[cfg(unix)]
        {
            Command::new("sleep").arg("30").spawn().unwrap()
        }
        #[cfg(windows)]
        {
            Command::new("cmd")
                .args(["/C", "ping", "-n", "30", "127.0.0.1"])
                .spawn()
                .unwrap()
        }
    }

    /// Reaps the child, failing the test if it does not exit in time.
    fn wait_for_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "child process was not terminated by terminate_processes"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// An empty set has no grace period because there is nothing to terminate.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_processes_empty_set_returns_without_signals_or_grace() {
        let mut recorder = Vec::new();

        tokio::time::timeout(
            Duration::from_millis(10),
            terminate_processes_with_signal_recorder(HashSet::new(), &mut recorder),
        )
        .await
        .expect("empty termination should not wait for the grace period");

        assert!(recorder.is_empty());
    }

    /// Both phases target the same complete pid set exactly once.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_processes_recorder_observes_term_then_kill_for_the_same_set() {
        use nix::sys::signal::Signal;

        let pids = HashSet::from([101, 202]);
        let mut recorder = Vec::new();

        terminate_processes_with_signal_recorder(pids.clone(), &mut recorder).await;

        assert_eq!(recorder.len(), pids.len() * 2);
        let (term_signals, kill_signals) = recorder.split_at(pids.len());
        assert_eq!(
            term_signals
                .iter()
                .map(|(pid, signal)| (*pid, *signal))
                .collect::<HashSet<_>>(),
            pids.iter()
                .map(|pid| (*pid, Signal::SIGTERM))
                .collect::<HashSet<_>>(),
        );
        assert_eq!(
            kill_signals
                .iter()
                .map(|(pid, signal)| (*pid, *signal))
                .collect::<HashSet<_>>(),
            pids.iter()
                .map(|pid| (*pid, Signal::SIGKILL))
                .collect::<HashSet<_>>(),
        );
    }

    /// [`terminate_processes`] must actually terminate the given processes on the platforms we
    /// support, not silently do nothing. Exercises the real (per-platform) kill path.
    #[tokio::test]
    async fn terminate_processes_terminates_the_given_pids() {
        let mut child = spawn_blocking_child();
        let pid = child.id() as i32;

        terminate_processes(std::iter::once(pid).collect()).await;

        let terminated = wait_for_exit(&mut child);

        assert!(
            !terminated.success(),
            "child should have been killed, but it exited cleanly"
        );
    }

    /// The `SIGTERM` phase is what lets an injected process run its own shutdown, so a process that
    /// handles `SIGTERM` must be allowed to exit on its own terms instead of being killed.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_processes_lets_a_cooperative_process_exit_on_sigterm() {
        use std::os::unix::process::ExitStatusExt;

        // `wait` (rather than a foreground `sleep`) is what makes the shell run the trap as soon as
        // the signal arrives, instead of after the current foreground command finishes.
        let mut child = spawn_ready_script("trap 'exit 7' TERM; echo ready; sleep 10 & wait");

        terminate_processes(std::iter::once(child.id() as i32).collect()).await;

        let terminated = wait_for_exit(&mut child);

        assert_eq!(
            terminated.code(),
            Some(7),
            "cooperative process should have exited through its own SIGTERM handler, \
             got signal {:?}",
            terminated.signal(),
        );
    }

    /// A process that ignores `SIGTERM` must still be gone after the grace period, otherwise it
    /// would keep holding its ports for the rest of the machine's life.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_processes_kills_a_sigterm_ignoring_process() {
        use std::os::unix::process::ExitStatusExt;

        let mut child = spawn_ready_script("trap '' TERM; echo ready; sleep 10");

        terminate_processes(std::iter::once(child.id() as i32).collect()).await;

        let terminated = wait_for_exit(&mut child);

        assert_eq!(
            terminated.signal(),
            Some(nix::sys::signal::Signal::SIGKILL as i32),
            "SIGTERM-ignoring process should have been killed after the grace period",
        );
    }
}
