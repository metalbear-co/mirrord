//! Keeps child ownership alive until shutdown has given services time to exit.

use std::{
    future::Future,
    io,
    ops::Not,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use futures::{StreamExt, stream::FuturesUnordered};
use mirrord_progress::messages::SESSION_READY_MESSAGE;
#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::{Child, Command},
    task::JoinSet,
};

use crate::{ReadyTracker, UpError};

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

struct Service {
    name: Arc<str>,
    child: Child,
    #[cfg(unix)]
    group: Pid,
}

impl Service {
    #[cfg(unix)]
    async fn stop(&mut self, grace: Duration) -> io::Result<()> {
        // Each group was created by us, never the user's foreground group.
        // A PGID only keeps identifying this group while some member is still
        // alive; the leader may already be reaped (normal exit, a crash, or
        // another service failing first), which frees the PGID for the OS to
        // reuse. Probe with a signal-less `killpg` immediately before every
        // real signal so a stale PGID number is never signalled blindly.
        let mut group_exists = self.probe()?;
        if group_exists {
            match killpg(self.group, Signal::SIGTERM) {
                Ok(()) | Err(Errno::ESRCH) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let deadline = tokio::time::Instant::now() + grace;
        while group_exists && tokio::time::Instant::now() < deadline {
            self.child.try_wait()?;
            group_exists = self.probe()?;
            if group_exists {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        if group_exists {
            match killpg(self.group, Signal::SIGKILL) {
                Ok(()) | Err(Errno::ESRCH) => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.child.wait().await?;
        Ok(())
    }

    /// Checks whether the process group still has a live member, without
    /// signalling it.
    #[cfg(unix)]
    fn probe(&self) -> io::Result<bool> {
        match killpg(self.group, None) {
            Ok(()) => Ok(true),
            Err(Errno::ESRCH) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    #[cfg(not(unix))]
    async fn stop(&mut self, _grace: Duration) -> io::Result<()> {
        // Tokio has no portable graceful process-termination primitive.
        if self.child.try_wait()?.is_none() {
            self.child.kill().await?;
        }
        self.child.wait().await?;
        Ok(())
    }
}

#[cfg(unix)]
fn shutdown_signal() -> io::Result<impl Future<Output = io::Result<()>>> {
    // Register before spawning children, not on the first poll of the waiter.
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    Ok(async move {
        let received = tokio::select! {
            received = interrupt.recv() => received,
            received = terminate.recv() => received,
            received = hangup.recv() => received,
        };
        received.ok_or_else(|| io::Error::other("shutdown signal stream closed"))
    })
}

#[cfg(windows)]
fn shutdown_signal() -> io::Result<impl Future<Output = io::Result<()>>> {
    let mut interrupt = tokio::signal::windows::ctrl_c()?;
    let mut ctrl_break = tokio::signal::windows::ctrl_break()?;
    Ok(async move {
        let received = tokio::select! {
            received = interrupt.recv() => received,
            received = ctrl_break.recv() => received,
        };
        received.ok_or_else(|| io::Error::other("shutdown signal stream closed"))
    })
}

pub(super) async fn run(
    commands: Vec<(Arc<str>, Command)>,
    ready: ReadyTracker,
) -> Result<(), UpError> {
    let shutdown = shutdown_signal()?;
    supervise(commands, ready, shutdown, SHUTDOWN_GRACE).await
}

async fn forward_output(
    stream: impl AsyncRead + Unpin,
    name: Arc<str>,
    readiness: Option<(Arc<AtomicUsize>, usize, Instant, ReadyTracker)>,
) {
    let mut lines = BufReader::new(stream).lines();
    let mut counted = false;
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if counted.not() && line.trim() == SESSION_READY_MESSAGE {
                    counted = true;
                    if let Some((count, total, start, ready)) = &readiness
                        && count.fetch_add(1, Ordering::Relaxed) + 1 == *total
                    {
                        ready
                            .elapsed
                            .set(start.elapsed())
                            .expect("only the final task sets readiness");
                    }
                }
                println!("{name}: {line}");
            }
            Ok(None) => break,
            Err(error) => {
                eprintln!("{name} output error: {error}");
                break;
            }
        }
    }
}

async fn supervise(
    commands: Vec<(Arc<str>, Command)>,
    ready: ReadyTracker,
    shutdown: impl Future<Output = io::Result<()>>,
    grace: Duration,
) -> Result<(), UpError> {
    let start = Instant::now();
    let total = commands.len();
    let count = Arc::new(AtomicUsize::new(0));
    let mut output = JoinSet::new();
    let mut services = Vec::with_capacity(total);
    let mut spawn_error = None;

    for (name, mut command) in commands {
        // Terminal Ctrl-C must reach the supervisor first so child signal exits
        // cannot race it and become spurious service-crash telemetry.
        #[cfg(unix)]
        command.process_group(0);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                spawn_error = Some(error);
                break;
            }
        };
        #[cfg(unix)]
        let group = Pid::from_raw(child.id().expect("spawned child has a PID") as i32);
        if let Some(stdout) = child.stdout.take() {
            output.spawn(forward_output(
                stdout,
                name.clone(),
                Some((count.clone(), total, start, ready.clone())),
            ));
        }
        if let Some(stderr) = child.stderr.take() {
            output.spawn(forward_output(stderr, name.clone(), None));
        }
        services.push(Service {
            name,
            child,
            #[cfg(unix)]
            group,
        });
    }

    let result = if let Some(error) = spawn_error {
        Err(UpError::Io(error))
    } else {
        let mut exits = services
            .iter_mut()
            .map(|service| async {
                let status = service.child.wait().await?;
                if status.success() {
                    Ok(())
                } else {
                    Err(UpError::ServiceCrashed {
                        name: service.name.clone(),
                        status,
                    })
                }
            })
            .collect::<FuturesUnordered<_>>();
        tokio::select! {
            biased;
            result = shutdown => result.map_err(UpError::Io),
            result = exits.next() => result.unwrap_or(Ok(())),
        }
    };

    // The same teardown applies to intentional stops, natural exits, and errors.
    // Keep forwarding pipes throughout the grace period so shutdown logs cannot
    // fill a pipe and prevent a cooperative child from exiting.
    let cleanup =
        futures::future::join_all(services.iter_mut().map(|service| service.stop(grace))).await;
    let cleanup_error = cleanup.into_iter().find_map(Result::err);
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        while output.join_next().await.is_some() {}
    })
    .await;
    output.shutdown().await;

    // A failed cleanup is useful diagnostic information, not evidence that a
    // deliberate user stop or an already-recorded service crash changed outcome.
    if let Some(error) = cleanup_error {
        eprintln!("Failed to stop a mirrord up child: {error}");
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use std::{path::Path, process::Stdio};

    use nix::sys::signal::kill;
    use rstest::rstest;
    use tempfile::TempDir;

    use super::*;

    const TEST_GRACE: Duration = Duration::from_millis(200);

    fn command(script: &str, directory: &Path) -> (Arc<str>, Command) {
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .current_dir(directory)
            .env("READY_MESSAGE", SESSION_READY_MESSAGE)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        (Arc::from("test"), command)
    }

    async fn wait_for_file(path: &Path) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while path.exists().not() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("child did not start");
    }

    #[tokio::test]
    async fn cancellation_allows_graceful_exit_and_preserves_readiness() {
        let directory = TempDir::new().unwrap();
        let ready = ReadyTracker::default();
        let cancel_ready = ready.clone();
        let command = command(
            "trap 'echo stopped > stopped; exit 0' TERM; echo \"$READY_MESSAGE\"; while :; do sleep 0.02; done",
            directory.path(),
        );
        let shutdown = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while cancel_ready.time_to_ready().is_none() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            Ok(())
        };
        assert!(
            supervise(vec![command], ready.clone(), shutdown, TEST_GRACE)
                .await
                .is_ok()
        );
        assert!(directory.path().join("stopped").exists());
        assert!(ready.time_to_ready().is_some());
    }

    #[tokio::test]
    async fn cancellation_before_readiness_escalates_uncooperative_child() {
        let directory = TempDir::new().unwrap();
        let ready = ReadyTracker::default();
        let command = command(
            "trap '' TERM; echo $$ > pid; touch started; exec sleep 60",
            directory.path(),
        );
        let shutdown = async {
            wait_for_file(&directory.path().join("started")).await;
            Ok(())
        };
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            supervise(vec![command], ready.clone(), shutdown, TEST_GRACE),
        )
        .await
        .expect("shutdown did not escalate");
        assert!(result.is_ok());
        assert!(ready.time_to_ready().is_none());
        let pid = std::fs::read_to_string(directory.path().join("pid")).unwrap();
        assert_eq!(
            kill(Pid::from_raw(pid.trim().parse().unwrap()), None),
            Err(Errno::ESRCH)
        );
    }

    #[tokio::test]
    async fn cleanup_reaches_grandchild_that_outlives_the_leader() {
        // Pins the actual capability of `killpg`: the leader can exit on its
        // own while an uncooperative grandchild it backgrounded keeps
        // running in the same process group. A `kill()`-only implementation
        // (signalling just the direct child) would pass every other test in
        // this module but leave the grandchild behind.
        let directory = TempDir::new().unwrap();
        let command = command(
            "trap '' TERM; (trap '' TERM; echo $$ > grandchild_pid; exec sleep 60) & disown; exec true",
            directory.path(),
        );
        // The leader exits on its own here (`exec true`), so `exits.next()`
        // resolves the select, not `shutdown`; the grandchild left in the
        // group is what exercises the reaped-leader probe path from `stop`.
        tokio::time::timeout(
            Duration::from_secs(3),
            supervise(
                vec![command],
                ReadyTracker::default(),
                std::future::pending(),
                TEST_GRACE,
            ),
        )
        .await
        .expect("cleanup did not complete")
        .expect("leader exiting on its own must not be reported as a crash");
        let pid = std::fs::read_to_string(directory.path().join("grandchild_pid")).unwrap();
        assert_eq!(
            kill(Pid::from_raw(pid.trim().parse().unwrap()), None),
            Err(Errno::ESRCH),
            "grandchild left in the process group must be cleaned up too"
        );
    }

    #[tokio::test]
    async fn crash_remains_failure_and_stops_siblings() {
        let directory = TempDir::new().unwrap();
        let sibling = command(
            "trap 'touch stopped; exit 0' TERM; touch started; while :; do sleep 0.02; done",
            directory.path(),
        );
        let failing = command(
            "while [ ! -f started ]; do sleep 0.02; done; exit 7",
            directory.path(),
        );
        let result = supervise(
            vec![sibling, failing],
            ReadyTracker::default(),
            std::future::pending(),
            TEST_GRACE,
        )
        .await;
        assert!(
            matches!(result, Err(UpError::ServiceCrashed { status, .. }) if status.code() == Some(7))
        );
        assert!(directory.path().join("stopped").exists());
    }

    #[tokio::test]
    async fn closed_output_does_not_starve_cancellation() {
        let directory = TempDir::new().unwrap();
        let command = command(
            "exec 1>&- 2>&-; touch started; exec sleep 60",
            directory.path(),
        );
        let shutdown = async {
            wait_for_file(&directory.path().join("started")).await;
            Ok(())
        };
        assert!(
            tokio::time::timeout(
                Duration::from_secs(3),
                supervise(vec![command], ReadyTracker::default(), shutdown, TEST_GRACE),
            )
            .await
            .unwrap()
            .is_ok()
        );
    }

    #[tokio::test]
    async fn spawn_failure_is_returned_instead_of_panicking() {
        let directory = TempDir::new().unwrap();
        let valid = command("exec sleep 60", directory.path());
        let missing = (
            Arc::from("missing"),
            Command::new(directory.path().join("missing")),
        );
        let result = supervise(
            vec![valid, missing],
            ReadyTracker::default(),
            std::future::pending(),
            TEST_GRACE,
        )
        .await;
        assert!(
            matches!(result, Err(UpError::Io(error)) if error.kind() == io::ErrorKind::NotFound)
        );
    }

    // Actual OS signals are only sent to this isolated test process, never to
    // the shared test harness (Tokio's signal handlers are process-global).
    #[tokio::test]
    #[ignore = "subprocess entry point for catches_shutdown_signals"]
    async fn signal_helper() {
        let directory = std::env::var_os("MIRRORD_UP_SIGNAL_TEST_DIRECTORY").unwrap();
        let directory = Path::new(&directory);
        let ready = ReadyTracker::default();
        let command = command(
            "trap 'touch stopped; exit 0' TERM; echo \"$READY_MESSAGE\"; while :; do sleep 0.02; done",
            directory,
        );
        run(vec![command], ready.clone()).await.unwrap();
        assert!(ready.time_to_ready().is_some());
        assert!(directory.join("stopped").exists());
    }

    #[rstest]
    #[case(Signal::SIGINT, false)]
    #[case(Signal::SIGTERM, false)]
    #[case(Signal::SIGHUP, false)]
    #[case(Signal::SIGINT, true)]
    #[tokio::test]
    async fn catches_shutdown_signals(#[case] signal: Signal, #[case] foreground_group: bool) {
        let directory = TempDir::new().unwrap();
        let mut helper = Command::new(std::env::current_exe().unwrap());
        helper
            .args([
                "--exact",
                "process::tests::signal_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("MIRRORD_UP_SIGNAL_TEST_DIRECTORY", directory.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0)
            .kill_on_drop(true);
        let mut helper = helper.spawn().unwrap();
        let pid = Pid::from_raw(helper.id().unwrap() as i32);
        let mut lines = BufReader::new(helper.stdout.take().unwrap()).lines();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let line = lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("helper exited before readiness");
                if line.contains(SESSION_READY_MESSAGE) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        if foreground_group {
            killpg(pid, signal).unwrap();
        } else {
            kill(pid, signal).unwrap();
        }
        let status = tokio::time::timeout(Duration::from_secs(8), helper.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success(), "helper failed: {status}");
    }
}
