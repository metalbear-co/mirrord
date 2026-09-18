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

use mirrord_progress::messages::SESSION_READY_MESSAGE;
#[cfg(unix)]
use nix::sys::signal::killpg;
#[cfg(unix)]
use nix::{errno::Errno, sys::signal::Signal, unistd::Pid};
#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
    sync::oneshot,
    task::JoinSet,
};

use crate::{ReadyTracker, UpError};

mod platform;
use platform::{ShutdownSignal, SignalStreams, receive_signal, signal_streams};

// Container runtimes conventionally allow ten seconds for graceful shutdown.
// Matching that window lets container services clean up before escalation
// kills the runtime client that is responsible for removing the container.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Records when every service has emitted its session-ready marker.
///
/// Stdout forwarding happens concurrently, so readiness is shared by all
/// forwarding tasks and must publish the elapsed time exactly once.
struct Readiness {
    /// Number of distinct service stdout streams that have reported readiness.
    count: AtomicUsize,
    /// Number of services that must become ready before the session is ready.
    total: usize,
    /// Start of service supervision, used to measure aggregate readiness time.
    start: Instant,
    /// Publishes the elapsed time to analytics and other readiness consumers.
    tracker: ReadyTracker,
}

impl Readiness {
    /// Marks one service ready and publishes timing when the final service arrives.
    ///
    /// Each stdout task calls this at most once. Relaxed ordering is sufficient
    /// because the counter only elects the final task; `ReadyTracker` provides
    /// the synchronization needed to publish and read the elapsed duration.
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

/// Owns a service process until its process tree has been shut down and reaped.
///
/// process-wrap creates a Unix process group or Windows Job Object for each
/// service, so teardown reaches descendants that outlive the direct child. The
/// Unix group ID remains available for its signal-zero liveness probe, which
/// process-wrap deliberately does not expose.
struct Service {
    /// Name used when reporting an unexpected service exit.
    name: Arc<str>,
    /// Wrapped child retained so supervision and teardown use tree-aware operations.
    child: Box<dyn ChildWrapper>,
    /// Unix process-group ID retained to probe surviving descendants.
    #[cfg(unix)]
    group: Pid,
}

impl Service {
    /// Gracefully signals the Unix process group, escalating after `grace`.
    ///
    /// The group is checked independently of the direct child because a
    /// descendant can remain alive after the group leader exits.
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
            self.kill_group()?;
        }
        self.child.wait().await?;
        Ok(())
    }

    /// Sends `signal` through process-wrap to the Unix process group.
    ///
    /// A missing group is a successful cleanup outcome rather than an error.
    #[cfg(unix)]
    fn signal_group(&self, signal: Signal) -> io::Result<bool> {
        match self.child.signal(signal as i32) {
            Ok(()) => Ok(true),
            Err(error) if error.raw_os_error() == Some(Errno::ESRCH as i32) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Escalates through process-wrap so SIGKILL reaches the Unix process group.
    #[cfg(unix)]
    fn kill_group(&mut self) -> io::Result<bool> {
        match self.child.start_kill() {
            Ok(()) => Ok(true),
            Err(error) if error.raw_os_error() == Some(Errno::ESRCH as i32) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Immediately terminates the process group and reaps its direct child.
    #[cfg(unix)]
    async fn force_stop(&mut self) -> io::Result<()> {
        self.kill_group()?;
        self.child.wait().await?;
        Ok(())
    }

    /// Probes whether any process still belongs to the Unix process group.
    #[cfg(unix)]
    fn group_exists(&self) -> io::Result<bool> {
        match killpg(self.group, None) {
            Ok(()) => Ok(true),
            Err(Errno::ESRCH) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Terminates and reaps the Windows Job Object.
    ///
    /// The grace period and received event cannot be forwarded as Windows
    /// console events, so Job Object termination preserves forceful shutdown
    /// while covering every descendant in the service tree.
    #[cfg(windows)]
    async fn stop(
        &mut self,
        _received_signal: Option<ShutdownSignal>,
        _grace: Duration,
    ) -> io::Result<()> {
        self.force_stop().await
    }

    /// Job Object termination reaches every descendant before the direct child
    /// is reaped, matching the Unix forced-shutdown guarantee.
    #[cfg(windows)]
    async fn force_stop(&mut self) -> io::Result<()> {
        Box::into_pin(self.child.kill()).await?;
        Ok(())
    }
}

struct SignalDelivery {
    signal: ShutdownSignal,
    accepted: oneshot::Sender<()>,
}

struct ShutdownSignals {
    first: oneshot::Receiver<io::Result<SignalDelivery>>,
    second: oneshot::Receiver<io::Result<SignalDelivery>>,
}

async fn watch_signals(
    mut signals: SignalStreams,
    first_sender: oneshot::Sender<io::Result<SignalDelivery>>,
    second_sender: oneshot::Sender<io::Result<SignalDelivery>>,
) {
    let signal = match receive_signal(&mut signals).await {
        Ok(signal) => signal,
        Err(error) => {
            let _ = first_sender.send(Err(error));
            return;
        }
    };
    let (accepted, acceptance) = oneshot::channel();
    if first_sender
        .send(Ok(SignalDelivery { signal, accepted }))
        .is_err()
        || acceptance.await.is_err()
    {
        std::process::exit(signal.forced_exit_code());
    }

    match receive_signal(&mut signals).await {
        Ok(signal) => {
            let (accepted, acceptance) = oneshot::channel();
            if second_sender
                .send(Ok(SignalDelivery { signal, accepted }))
                .is_err()
                || acceptance.await.is_err()
            {
                std::process::exit(signal.forced_exit_code());
            }
        }
        Err(error) => {
            let _ = second_sender.send(Err(error));
        }
    }
}

fn shutdown_signals() -> io::Result<ShutdownSignals> {
    // Register before spawning children, not on the first poll of either
    // receiver. Both receivers exist before the watcher starts, so a second
    // signal remains pending until supervision can force every service tree.
    let signals = signal_streams()?;
    let (first_sender, first) = oneshot::channel();
    let (second_sender, second) = oneshot::channel();
    tokio::spawn(watch_signals(signals, first_sender, second_sender));
    Ok(ShutdownSignals { first, second })
}

async fn receive_first_signal(
    receiver: oneshot::Receiver<io::Result<SignalDelivery>>,
) -> io::Result<ShutdownSignal> {
    let delivery = receiver
        .await
        .map_err(|_| io::Error::other("shutdown signal task stopped"))??;
    let _ = delivery.accepted.send(());
    Ok(delivery.signal)
}

async fn receive_second_signal(
    receiver: oneshot::Receiver<io::Result<SignalDelivery>>,
) -> io::Result<SignalDelivery> {
    receiver
        .await
        .map_err(|_| io::Error::other("shutdown signal task stopped"))?
}

pub(super) async fn run(
    commands: Vec<(Arc<str>, Command)>,
    ready: ReadyTracker,
) -> Result<(), UpError> {
    let shutdown = shutdown_signals()?;
    let (result, forced_signal) = supervise_with_second_signal(
        commands,
        ready,
        receive_first_signal(shutdown.first),
        receive_second_signal(shutdown.second),
        SHUTDOWN_GRACE,
    )
    .await?;
    if let Some(signal) = forced_signal {
        let exit_code = signal.signal.forced_exit_code();
        let _ = signal.accepted.send(());
        std::process::exit(exit_code);
    }
    result
}

async fn wait_for_first_exit(services: &mut [Service]) -> Result<(), UpError> {
    loop {
        for service in &mut *services {
            if let Some(status) = service.child.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(UpError::ServiceCrashed {
                        name: service.name.clone(),
                        status,
                    })
                };
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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

#[cfg(all(test, unix))]
async fn supervise(
    commands: Vec<(Arc<str>, Command)>,
    ready: ReadyTracker,
    shutdown: impl Future<Output = io::Result<ShutdownSignal>>,
    grace: Duration,
) -> Result<(), UpError> {
    supervise_with_second_signal(commands, ready, shutdown, std::future::pending(), grace)
        .await?
        .0
}

async fn supervise_with_second_signal(
    commands: Vec<(Arc<str>, Command)>,
    ready: ReadyTracker,
    shutdown: impl Future<Output = io::Result<ShutdownSignal>>,
    second_shutdown: impl Future<Output = io::Result<SignalDelivery>>,
    grace: Duration,
) -> Result<(Result<(), UpError>, Option<SignalDelivery>), UpError> {
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

    for (name, command) in commands {
        // Terminal Ctrl-C must reach the supervisor first so child signal exits
        // cannot race it and become spurious service-crash telemetry.
        let mut command = CommandWrap::from(command);
        command.wrap(KillOnDrop);
        #[cfg(unix)]
        command.wrap(ProcessGroup::leader());
        #[cfg(windows)]
        command.wrap(JobObject);
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
        if let Some(stdout) = child.stdout().take() {
            output.spawn(forward_output(
                stdout,
                name.clone(),
                Some(readiness.clone()),
            ));
        }
        if let Some(stderr) = child.stderr().take() {
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
        {
            let exits = wait_for_first_exit(&mut services);
            tokio::select! {
                // A deliberate signal wins a same-poll tie so the session remains
                // successful for telemetry regardless of a coincident child exit.
                biased;
                result = shutdown => match result {
                    Ok(signal) => (Ok(()), Some(signal)),
                    Err(error) => (Err(UpError::Io(error)), None),
                },
                result = exits => (result, None),
            }
        }
    };

    // The same teardown applies to intentional stops, natural exits, and errors.
    // Keep forwarding pipes throughout the grace period so shutdown logs cannot
    // fill a pipe and prevent a cooperative child from exiting.
    let cleanup = Box::pin(futures::future::join_all(
        services
            .iter_mut()
            .map(|service| service.stop(received_signal, grace)),
    ));
    // Keep this receiver alive through output cleanup too. A second signal can
    // arrive after the service trees finish but before supervision returns.
    tokio::pin!(second_shutdown);
    let (cleanup, forced_signal, second_shutdown_error) = tokio::select! {
        cleanup = cleanup => (Some(cleanup), None, None),
        signal = &mut second_shutdown => match signal {
            Ok(signal) => (None, Some(signal), None),
            Err(error) => (None, None, Some(error)),
        },
    };
    let cleanup = match cleanup {
        Some(cleanup) => cleanup,
        None => futures::future::join_all(services.iter_mut().map(Service::force_stop)).await,
    };
    let cleanup_error = cleanup.into_iter().find_map(Result::err);
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        while output.join_next().await.is_some() {}
    })
    .await;
    output.shutdown().await;

    let (forced_signal, second_shutdown_error) = if let Some(signal) = forced_signal {
        (Some(signal), None)
    } else if let Some(error) = second_shutdown_error {
        (None, Some(error))
    } else {
        tokio::select! {
            biased;
            signal = &mut second_shutdown => match signal {
                Ok(signal) => (Some(signal), None),
                Err(error) => (None, Some(error)),
            },
            _ = tokio::task::yield_now() => (None, None),
        }
    };

    // A failed cleanup is useful diagnostic information, not evidence that a
    // deliberate user stop or an already-recorded service crash changed outcome.
    if let Some(error) = cleanup_error {
        eprintln!("Failed to stop a mirrord up child: {error}");
    }
    if let Some(error) = second_shutdown_error {
        return Err(UpError::Io(error));
    }
    Ok((result, forced_signal))
}

#[cfg(all(test, unix))]
mod tests {
    use std::{path::Path, process::Stdio};

    use nix::sys::signal::kill;
    use rstest::rstest;
    use tempfile::TempDir;
    use tokio::process::Child;

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
    async fn second_shutdown_error_forces_cleanup_before_returning() {
        let directory = TempDir::new().unwrap();
        let command = command(
            "trap '' TERM; echo $$ > pid; touch started; exec sleep 60",
            directory.path(),
        );
        let first_shutdown = async {
            wait_for_file(&directory.path().join("started")).await;
            Ok(ShutdownSignal::Terminate)
        };
        let second_shutdown = async {
            wait_for_file(&directory.path().join("started")).await;
            Err(io::Error::other("shutdown signal stream closed"))
        };
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            supervise_with_second_signal(
                vec![command],
                ReadyTracker::default(),
                first_shutdown,
                second_shutdown,
                TEST_GRACE,
            ),
        )
        .await
        .expect("second shutdown error did not force cleanup");
        assert!(matches!(result, Err(UpError::Io(error)) if error.kind() == io::ErrorKind::Other));
        let pid = Pid::from_raw(
            std::fs::read_to_string(directory.path().join("pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        );
        assert_eq!(kill(pid, None), Err(Errno::ESRCH));
        assert_eq!(killpg(pid, None), Err(Errno::ESRCH));
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
        // The leader exits on its own here (`exec true`), so first-exit polling
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
    // the shared test harness (tokio's signal handlers are process-global).
    #[tokio::test]
    #[ignore = "subprocess entry point for signal tests"]
    async fn signal_helper() {
        let Some(directory) = std::env::var_os("MIRRORD_UP_SIGNAL_TEST_DIRECTORY") else {
            return;
        };
        let directory = Path::new(&directory);
        let ready = ReadyTracker::default();
        let natural_exit = std::env::var_os("MIRRORD_UP_SIGNAL_TEST_NATURAL_EXIT").is_some();
        let ignores_term = std::env::var_os("MIRRORD_UP_SIGNAL_TEST_IGNORES_TERM").is_some();
        let command = command(
            if natural_exit {
                "echo \"$READY_MESSAGE\"; exit 0"
            } else if ignores_term {
                "trap 'touch first-term' TERM; echo $$ > child_pid; echo \"$READY_MESSAGE\"; while :; do sleep 0.02; done"
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

    fn signal_test_helper(
        directory: &Path,
        drain: bool,
        natural_exit: bool,
        ignores_term: bool,
    ) -> Child {
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
        if ignores_term {
            helper.env("MIRRORD_UP_SIGNAL_TEST_IGNORES_TERM", "1");
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
        let mut helper = signal_test_helper(directory.path(), false, false, false);
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
    async fn second_signal_forces_cleanup_before_exit() {
        let directory = TempDir::new().unwrap();
        let mut helper = signal_test_helper(directory.path(), false, false, true);
        let pid = Pid::from_raw(helper.id().unwrap() as i32);
        wait_for_helper_ready(&mut helper).await;
        let child = Pid::from_raw(
            std::fs::read_to_string(directory.path().join("child_pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap(),
        );
        kill(pid, Signal::SIGTERM).unwrap();
        wait_for_file(&directory.path().join("first-term")).await;
        kill(pid, Signal::SIGINT).unwrap();
        let status = tokio::time::timeout(Duration::from_secs(3), helper.wait())
            .await
            .expect("second signal did not force cleanup")
            .unwrap();
        assert_eq!(status.code(), Some(130));
        assert_eq!(kill(child, None), Err(Errno::ESRCH));
        assert_eq!(killpg(child, None), Err(Errno::ESRCH));
    }

    #[tokio::test]
    async fn one_signal_forces_exit_after_natural_service_exit() {
        let directory = TempDir::new().unwrap();
        let mut helper = signal_test_helper(directory.path(), true, true, false);
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
