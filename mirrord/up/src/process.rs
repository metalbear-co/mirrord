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
    sync::oneshot,
    task::JoinSet,
};

use crate::{ReadyTracker, UpError};

// Container runtimes conventionally allow ten seconds for graceful shutdown.
// Matching that window lets container services clean up before escalation
// kills the runtime client that is responsible for removing the container.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
enum ShutdownSignal {
    #[cfg(unix)]
    Interrupt,
    #[cfg(unix)]
    Terminate,
    #[cfg(unix)]
    Hangup,
    #[cfg(windows)]
    CtrlC,
    #[cfg(windows)]
    CtrlBreak,
}

impl ShutdownSignal {
    #[cfg(unix)]
    fn unix_signal(self) -> Signal {
        match self {
            Self::Interrupt => Signal::SIGINT,
            Self::Terminate => Signal::SIGTERM,
            Self::Hangup => Signal::SIGHUP,
        }
    }

    fn exit_code(self) -> i32 {
        match self {
            #[cfg(unix)]
            Self::Interrupt => 130,
            #[cfg(unix)]
            Self::Terminate => 143,
            #[cfg(unix)]
            Self::Hangup => 129,
            #[cfg(windows)]
            Self::CtrlC | Self::CtrlBreak => 130,
        }
    }
}

struct Readiness {
    count: AtomicUsize,
    total: usize,
    start: Instant,
    tracker: ReadyTracker,
}

impl Readiness {
    fn mark(&self) {
        if self.count.fetch_add(1, Ordering::Relaxed) + 1 == self.total {
            // TODO(areg) downgrade to a debug_assert once the feature stabilizes.
            self.tracker
                .elapsed
                .set(self.start.elapsed())
                .expect("only the final task sets readiness");
        }
    }
}

struct Service {
    name: Arc<str>,
    child: Child,
    #[cfg(unix)]
    group: Pid,
}

impl Service {
    #[cfg(unix)]
    async fn stop(
        &mut self,
        received_signal: Option<ShutdownSignal>,
        grace: Duration,
    ) -> io::Result<()> {
        // ESRCH is benign. If the leader was already reaped, PGID reuse is an
        // unavoidable OS race; retaining the PGID is still required to reach
        // an original grandchild that outlives that leader.
        let signal = received_signal
            .map(ShutdownSignal::unix_signal)
            .unwrap_or(Signal::SIGTERM);
        let mut group_exists = self.group_exists()?;
        if group_exists {
            group_exists = self.signal_group(signal)?;
        }
        let deadline = tokio::time::Instant::now() + grace;
        while group_exists && tokio::time::Instant::now() < deadline {
            self.child.try_wait()?;
            group_exists = self.group_exists()?;
            if group_exists {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        if group_exists {
            self.signal_group(Signal::SIGKILL)?;
        }
        self.child.wait().await?;
        Ok(())
    }

    #[cfg(unix)]
    fn signal_group(&self, signal: Signal) -> io::Result<bool> {
        match killpg(self.group, signal) {
            Ok(()) => Ok(true),
            Err(Errno::ESRCH) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    #[cfg(unix)]
    fn group_exists(&self) -> io::Result<bool> {
        match killpg(self.group, None) {
            Ok(()) => Ok(true),
            Err(Errno::ESRCH) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    #[cfg(windows)]
    async fn stop(
        &mut self,
        _received_signal: Option<ShutdownSignal>,
        _grace: Duration,
    ) -> io::Result<()> {
        // Tokio has no portable graceful process-termination primitive.
        if self.child.try_wait()?.is_none() {
            self.child.kill().await?;
        }
        self.child.wait().await?;
        Ok(())
    }
}

#[cfg(unix)]
type SignalStreams = (
    tokio::signal::unix::Signal,
    tokio::signal::unix::Signal,
    tokio::signal::unix::Signal,
);

#[cfg(unix)]
fn signal_streams() -> io::Result<SignalStreams> {
    Ok((
        signal(SignalKind::interrupt())?,
        signal(SignalKind::terminate())?,
        signal(SignalKind::hangup())?,
    ))
}

#[cfg(unix)]
async fn receive_signal(signals: &mut SignalStreams) -> io::Result<ShutdownSignal> {
    tokio::select! {
        received = signals.0.recv() => received.map(|_| ShutdownSignal::Interrupt),
        received = signals.1.recv() => received.map(|_| ShutdownSignal::Terminate),
        received = signals.2.recv() => received.map(|_| ShutdownSignal::Hangup),
    }
    .ok_or_else(|| io::Error::other("shutdown signal stream closed"))
}

#[cfg(windows)]
type SignalStreams = (
    tokio::signal::windows::CtrlC,
    tokio::signal::windows::CtrlBreak,
);

#[cfg(windows)]
fn signal_streams() -> io::Result<SignalStreams> {
    Ok((
        tokio::signal::windows::ctrl_c()?,
        tokio::signal::windows::ctrl_break()?,
    ))
}

#[cfg(windows)]
async fn receive_signal(signals: &mut SignalStreams) -> io::Result<ShutdownSignal> {
    tokio::select! {
        received = signals.0.recv() => received.map(|_| ShutdownSignal::CtrlC),
        received = signals.1.recv() => received.map(|_| ShutdownSignal::CtrlBreak),
    }
    .ok_or_else(|| io::Error::other("shutdown signal stream closed"))
}

struct SignalDelivery {
    signal: ShutdownSignal,
    accepted: oneshot::Sender<()>,
}

async fn watch_signals(
    mut signals: SignalStreams,
    sender: oneshot::Sender<io::Result<SignalDelivery>>,
) {
    let signal = match receive_signal(&mut signals).await {
        Ok(signal) => signal,
        Err(error) => {
            let _ = sender.send(Err(error));
            return;
        }
    };
    let (accepted, acceptance) = oneshot::channel();
    if sender
        .send(Ok(SignalDelivery { signal, accepted }))
        .is_err()
        || acceptance.await.is_err()
    {
        std::process::exit(signal.exit_code());
    }

    if let Ok(signal) = receive_signal(&mut signals).await {
        std::process::exit(signal.exit_code());
    }
}

fn shutdown_signal() -> io::Result<impl Future<Output = io::Result<ShutdownSignal>>> {
    // Register before spawning children, not on the first poll of the waiter.
    // Tokio keeps these handlers installed, so a signal that has no accepting
    // supervisor and a second accepted signal both force an immediate exit.
    let signals = signal_streams()?;
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(watch_signals(signals, sender));
    Ok(async move {
        let delivery = receiver
            .await
            .map_err(|_| io::Error::other("shutdown signal task stopped"))??;
        let _ = delivery.accepted.send(());
        Ok(delivery.signal)
    })
}

#[cfg(not(any(unix, windows)))]
compile_error!("mirrord up process supervision supports only Unix and Windows");

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
    readiness: Option<Arc<Readiness>>,
) {
    let mut lines = BufReader::new(stream).lines();
    let mut counted = false;
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if counted.not() && line.trim() == SESSION_READY_MESSAGE {
                    counted = true;
                    if let Some(readiness) = &readiness {
                        readiness.mark();
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
    shutdown: impl Future<Output = io::Result<ShutdownSignal>>,
    grace: Duration,
) -> Result<(), UpError> {
    let total = commands.len();
    let readiness = Arc::new(Readiness {
        count: AtomicUsize::new(0),
        total,
        start: Instant::now(),
        tracker: ready,
    });
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
        let Some(group) = child.id().map(|pid| Pid::from_raw(pid as i32)) else {
            spawn_error = Some(io::Error::other("spawned child has no process ID"));
            break;
        };
        if let Some(stdout) = child.stdout.take() {
            output.spawn(forward_output(
                stdout,
                name.clone(),
                Some(readiness.clone()),
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

    let (result, received_signal) = if let Some(error) = spawn_error {
        // No select will poll this receiver, so dropping it marks any later
        // signal as unaccepted before cleanup begins.
        drop(shutdown);
        (Err(UpError::Io(error)), None)
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
            // A deliberate signal wins a same-poll tie so the session remains
            // successful for telemetry regardless of a coincident child exit.
            biased;
            result = shutdown => match result {
                Ok(signal) => (Ok(()), Some(signal)),
                Err(error) => (Err(UpError::Io(error)), None),
            },
            result = exits.next() => (result.unwrap_or(Ok(())), None),
        }
    };

    // The same teardown applies to intentional stops, natural exits, and errors.
    // Keep forwarding pipes throughout the grace period so shutdown logs cannot
    // fill a pipe and prevent a cooperative child from exiting.
    let cleanup = futures::future::join_all(
        services
            .iter_mut()
            .map(|service| service.stop(received_signal, grace)),
    )
    .await;
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
            Ok(ShutdownSignal::Terminate)
        };
        supervise(vec![command], ready.clone(), shutdown, TEST_GRACE)
            .await
            .unwrap();
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
            Ok(ShutdownSignal::Terminate)
        };
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            supervise(vec![command], ready.clone(), shutdown, TEST_GRACE),
        )
        .await
        .expect("shutdown did not escalate");
        result.unwrap();
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
            "trap '' TERM; (trap '' TERM; exec sleep 60) & echo $! > grandchild_pid; exec true",
            directory.path(),
        );
        // The leader exits on its own here (`exec true`), so `exits.next()`
        // resolves the select, not `shutdown`; the retained process-group ID
        // is the only remaining way to reach the grandchild.
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
        let pid = Pid::from_raw(pid.trim().parse().unwrap());
        let exited = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match kill(pid, None) {
                    Ok(()) => tokio::time::sleep(Duration::from_millis(10)).await,
                    Err(Errno::ESRCH) => break,
                    Err(error) => panic!("failed to inspect grandchild process: {error}"),
                }
            }
        })
        .await;
        assert!(
            exited.is_ok(),
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
            Ok(ShutdownSignal::Terminate)
        };
        tokio::time::timeout(
            Duration::from_secs(3),
            supervise(vec![command], ReadyTracker::default(), shutdown, TEST_GRACE),
        )
        .await
        .unwrap()
        .unwrap();
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
    #[ignore = "subprocess entry point for signal tests"]
    async fn signal_helper() {
        let Some(directory) = std::env::var_os("MIRRORD_UP_SIGNAL_TEST_DIRECTORY") else {
            return;
        };
        let directory = Path::new(&directory);
        let ready = ReadyTracker::default();
        let natural_exit = std::env::var_os("MIRRORD_UP_SIGNAL_TEST_NATURAL_EXIT").is_some();
        let command = command(
            if natural_exit {
                "echo \"$READY_MESSAGE\"; exit 0"
            } else {
                "trap 'touch stopped-int; exit 0' INT; trap 'touch stopped-term; exit 0' TERM; trap 'touch stopped-hup; exit 0' HUP; echo \"$READY_MESSAGE\"; while :; do sleep 0.02; done"
            },
            directory,
        );
        run(vec![command], ready.clone()).await.unwrap();
        assert!(ready.time_to_ready().is_some());
        if std::env::var_os("MIRRORD_UP_SIGNAL_TEST_DRAIN").is_some() {
            std::fs::write(directory.join("draining"), []).unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }

    async fn wait_for_helper_ready(helper: &mut Child) {
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
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    }

    fn signal_test_helper(directory: &Path, drain: bool, natural_exit: bool) -> Child {
        let mut helper = Command::new(std::env::current_exe().unwrap());
        helper
            .args([
                "--exact",
                "process::tests::signal_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("MIRRORD_UP_SIGNAL_TEST_DIRECTORY", directory)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0)
            .kill_on_drop(true);
        if drain {
            helper.env("MIRRORD_UP_SIGNAL_TEST_DRAIN", "1");
        }
        if natural_exit {
            helper.env("MIRRORD_UP_SIGNAL_TEST_NATURAL_EXIT", "1");
        }
        helper.spawn().unwrap()
    }

    #[rstest]
    #[case(Signal::SIGINT, "stopped-int", false)]
    #[case(Signal::SIGTERM, "stopped-term", false)]
    #[case(Signal::SIGHUP, "stopped-hup", false)]
    #[case(Signal::SIGINT, "stopped-int", true)]
    #[tokio::test]
    async fn catches_and_forwards_shutdown_signals(
        #[case] signal: Signal,
        #[case] stopped_file: &str,
        #[case] foreground_group: bool,
    ) {
        let directory = TempDir::new().unwrap();
        let mut helper = signal_test_helper(directory.path(), false, false);
        let pid = Pid::from_raw(helper.id().unwrap() as i32);
        wait_for_helper_ready(&mut helper).await;
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
        assert!(directory.path().join(stopped_file).exists());
    }

    #[tokio::test]
    async fn second_signal_forces_exit_after_supervision_returns() {
        let directory = TempDir::new().unwrap();
        let mut helper = signal_test_helper(directory.path(), true, false);
        let pid = Pid::from_raw(helper.id().unwrap() as i32);
        wait_for_helper_ready(&mut helper).await;
        kill(pid, Signal::SIGTERM).unwrap();
        wait_for_file(&directory.path().join("draining")).await;
        kill(pid, Signal::SIGINT).unwrap();
        let status = tokio::time::timeout(Duration::from_secs(3), helper.wait())
            .await
            .expect("second signal did not force exit")
            .unwrap();
        assert_eq!(status.code(), Some(130));
    }

    #[tokio::test]
    async fn one_signal_forces_exit_after_natural_service_exit() {
        let directory = TempDir::new().unwrap();
        let mut helper = signal_test_helper(directory.path(), true, true);
        let pid = Pid::from_raw(helper.id().unwrap() as i32);
        wait_for_helper_ready(&mut helper).await;
        wait_for_file(&directory.path().join("draining")).await;
        kill(pid, Signal::SIGTERM).unwrap();
        let status = tokio::time::timeout(Duration::from_secs(3), helper.wait())
            .await
            .expect("undeliverable signal did not force exit")
            .unwrap();
        assert_eq!(status.code(), Some(143));
    }
}
