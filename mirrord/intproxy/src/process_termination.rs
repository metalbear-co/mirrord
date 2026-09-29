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
use nix::{
    errno::Errno,
    sys::signal::{Signal, kill},
    unistd::Pid,
};
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

/// On Unix, sends `SIGKILL` to each registered layer process once.
///
/// Intproxy quiesces registrations before calling this function. A second signal after a grace
/// period could hit a different process if the registered PID was reused in between. A process
/// that already exited is the outcome we want, so `ESRCH` is success. Other signalling errors
/// are logged without leaving the remaining processes unsignalled.
#[cfg(unix)]
pub(crate) async fn terminate_processes(pids: HashSet<i32>) {
    terminate_processes_with(pids, send_signal).await;
}

#[cfg(unix)]
async fn terminate_processes_with(pids: HashSet<i32>, mut signal_process: impl FnMut(i32, Signal)) {
    if pids.is_empty() {
        return;
    }

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
    #[cfg(unix)]
    use std::collections::HashSet;
    use std::{process::Command, time::Duration};

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

    /// An empty set does not delay shutdown when there is nothing to terminate.
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

    /// A registered PID is signalled only once, so an exited process cannot be targeted again
    /// after its number has been reused.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_processes_recorder_observes_one_kill_per_pid() {
        use nix::sys::signal::Signal;

        let pids = HashSet::from([101, 202]);
        let mut recorder = Vec::new();

        terminate_processes_with_signal_recorder(pids.clone(), &mut recorder).await;

        assert_eq!(recorder.len(), pids.len());
        assert_eq!(
            recorder.into_iter().collect::<HashSet<_>>(),
            pids.into_iter()
                .map(|pid| (pid, Signal::SIGKILL))
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

    /// Even a cooperative layer is killed directly: a delayed escalation could target a reused
    /// numeric PID, while the intproxy itself remains graceful on `SIGTERM`.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_processes_kills_without_sigterm() {
        use std::os::unix::process::ExitStatusExt;

        let mut child = spawn_ready_script("trap 'exit 7' TERM; echo ready; while :; do :; done");

        terminate_processes(std::iter::once(child.id() as i32).collect()).await;

        let terminated = wait_for_exit(&mut child);
        assert_eq!(
            terminated.signal(),
            Some(nix::sys::signal::Signal::SIGKILL as i32)
        );
    }
}
