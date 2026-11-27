//! Keeps child ownership alive until shutdown has given services time to exit.

use std::{
    future::Future,
    io,
    ops::Not,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use mirrord_analytics::{UP_DB_BRANCHES_REPORT_PREFIX, UpSessionDbBranch};
use mirrord_debug_hold::{
    ANNOUNCEMENT_PREFIX, ENDED_PREFIX, PLAN_PREFIX, Trigger,
    runtime::{DiagnosticPort, Report},
};
#[cfg(test)]
use mirrord_progress::messages::SESSION_READY_MESSAGE;
use mirrord_progress::{is_progress_line, is_session_ready_line};
#[cfg(unix)]
use nix::sys::signal::killpg;
#[cfg(unix)]
use nix::{errno::Errno, sys::signal::Signal, unistd::Pid};
#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use serde_json::{
    Map, Value,
    value::{RawValue, to_raw_value},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::Command,
    sync::oneshot,
    task::JoinSet,
};

use crate::{
    ReadyTracker, UpError,
    output::{
        Event, ExitReason, OutputFormat, PreCommandStage, Printer, Remaining, ServiceState, Source,
        Stream, session_error, session_error_line,
    },
};

mod platform;
use platform::{ShutdownSignal, SignalStreams, receive_signal, signal_streams};

// Container runtimes conventionally allow ten seconds for graceful shutdown.
// Matching that window lets container services clean up before escalation
// kills the runtime client that is responsible for removing the container.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// How long an exited process's output is still read before saying how it ended, such as whether
/// a session got as far as
/// [`SESSION_READY_MESSAGE`](mirrord_progress::messages::SESSION_READY_MESSAGE).
///
/// The exit can be seen before the forwarding tasks have read the last lines, and a descendant
/// still holding a pipe open must not hold up the error.
///
/// Under `--output json`, it also gives a line after a session's error report the time to show the
/// report wasn't the session's last.
const OUTPUT_DRAIN: Duration = Duration::from_secs(1);

/// How long after a debugged session's program started `up` says that none of its .NET runtimes
/// has paused yet, when one was to. A runtime pauses as it starts, but `dotnet run` builds first,
/// so it's only a note: the runtime may still come.
const NO_RUNTIME_PAUSED_AFTER: Duration = Duration::from_secs(15);

/// Records when every service has emitted its session-ready marker.
///
/// Stdout forwarding happens concurrently, so readiness is shared by all
/// forwarding tasks and must publish the elapsed time exactly once. The
/// measured time includes `run.pre_command`s, and it is never published when a
/// service is dropped because its pre-command failed.
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
    /// Whether `mirrord up --debug` holds the service's program for a debugger. See
    /// [`ServiceCommands::debugged`].
    debugged: bool,
    /// The mirrord session to spawn once `child`, the service's
    /// `run.pre_command`, exits successfully. `None` once `child` is the
    /// session itself.
    then: Option<Command>,
    /// Wrapped child retained so supervision and teardown use tree-aware operations.
    child: Box<dyn ChildWrapper>,
    /// Unix process-group ID retained to probe surviving descendants.
    #[cfg(unix)]
    group: Pid,
    /// Set once the session printed
    /// [`SESSION_READY_MESSAGE`](mirrord_progress::messages::SESSION_READY_MESSAGE), to when it
    /// did, i.e. once it got as far as launching the service's process. Tells a session that
    /// failed to start from a service that crashed.
    started: Arc<OnceLock<Instant>>,
    /// Set once something of the service is held and an IDE may attach: the session printed its
    /// [`ANNOUNCEMENT_PREFIX`] line, or a .NET runtime paused on the service's diagnostic port.
    /// From then on a debugged program's exit is the debugger's doing.
    held: Arc<AtomicBool>,
    /// What the session's `debug-plan` line said holds its program, once it said it.
    planned: Arc<Mutex<Option<Trigger>>>,
    /// The final error the session's `mirrord exec` reported, under `--output json`, once its
    /// stderr closed.
    error: Arc<Mutex<Option<Box<RawValue>>>>,
    /// Whether `up` already said no .NET runtime paused while the service runs.
    runtime_note: RuntimeNote,
    /// Resolves once the forwarding of the child's stdout and stderr ended, so what they report is
    /// final.
    output_closed: oneshot::Receiver<()>,
    /// Writes what `up` says about the service.
    printer: Printer,
}

/// Whether [`Service::say_if_no_runtime_paused_yet`] said so already.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeNote {
    Pending,
    Said,
}

/// Which of a child's streams a forwarding task reads, and what it reports back.
enum Forwarded {
    /// A pre-command's stream, which reports nothing.
    PreCommand { stream: Stream, _closing: Closing },
    /// A session's stdout.
    SessionStdout(SessionOutput),
    /// A session's stderr.
    SessionStderr(SessionErrors),
}

/// Dropped by each of a child's two forwarding tasks when it ends; the last one dropped resolves
/// [`Service::output_closed`].
type Closing = Arc<oneshot::Sender<()>>;

/// What forwarding a session's stdout (not a pre-command's) reports back.
struct SessionOutput {
    readiness: Arc<Readiness>,
    started: Arc<OnceLock<Instant>>,
    held: Arc<AtomicBool>,
    planned: Arc<Mutex<Option<Trigger>>>,
    _closing: Closing,
}

/// What forwarding a session's stderr reports back.
struct SessionErrors {
    /// Under `--output json`, the session's final error: its last line so far, while that's an
    /// error report. Taking it makes it the error, and the line then isn't printed as output.
    error: Arc<Mutex<Option<Box<RawValue>>>>,
    _closing: Closing,
}

impl Forwarded {
    fn stream(&self) -> Stream {
        match self {
            Self::PreCommand { stream, .. } => *stream,
            Self::SessionStdout(_) => Stream::Stdout,
            Self::SessionStderr(_) => Stream::Stderr,
        }
    }

    fn source(&self) -> Source {
        match self {
            Self::PreCommand { .. } => Source::PreCommand,
            Self::SessionStdout(_) | Self::SessionStderr(_) => Source::Session,
        }
    }
}

impl Service {
    /// Spawns `command` in its own process tree and forwards its output.
    ///
    /// Only a session child's stdout carries the readiness marker, so a
    /// pre-command (one with a `then`) doesn't count toward `readiness`.
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        name: Arc<str>,
        debugged: bool,
        held: Arc<AtomicBool>,
        command: Command,
        then: Option<Command>,
        readiness: &Arc<Readiness>,
        output: &mut JoinSet<()>,
        printer: &Printer,
    ) -> io::Result<Self> {
        // Terminal Ctrl-C must reach the supervisor first so child signal exits
        // cannot race it and become spurious service-crash telemetry.
        let mut command = CommandWrap::from(command);
        command.wrap(KillOnDrop);
        #[cfg(unix)]
        command.wrap(ProcessGroup::leader());
        #[cfg(windows)]
        command.wrap(JobObject);
        let mut child = command.spawn()?;
        #[cfg(unix)]
        let group = child
            .id()
            .map(|pid| Pid::from_raw(pid as i32))
            .ok_or_else(|| io::Error::other("spawned child has no process ID"))?;
        let started = Arc::new(OnceLock::new());
        let planned = Arc::new(Mutex::new(None));
        let error = Arc::new(Mutex::new(None));
        let (closing, output_closed) = oneshot::channel();
        let closing = Arc::new(closing);
        if let Some(stdout) = child.stdout().take() {
            let forwarded = match then {
                Some(_) => Forwarded::PreCommand {
                    stream: Stream::Stdout,
                    _closing: closing.clone(),
                },
                None => Forwarded::SessionStdout(SessionOutput {
                    readiness: readiness.clone(),
                    started: started.clone(),
                    held: held.clone(),
                    planned: planned.clone(),
                    _closing: closing.clone(),
                }),
            };
            output.spawn(forward_output(
                stdout,
                name.clone(),
                printer.clone(),
                forwarded,
            ));
        }
        if let Some(stderr) = child.stderr().take() {
            let forwarded = match then {
                Some(_) => Forwarded::PreCommand {
                    stream: Stream::Stderr,
                    _closing: closing,
                },
                None => Forwarded::SessionStderr(SessionErrors {
                    error: error.clone(),
                    _closing: closing,
                }),
            };
            output.spawn(forward_output(
                stderr,
                name.clone(),
                printer.clone(),
                forwarded,
            ));
        }
        Ok(Self {
            name,
            debugged,
            then,
            child,
            #[cfg(unix)]
            group,
            started,
            held,
            planned,
            error,
            runtime_note: RuntimeNote::Pending,
            output_closed,
            printer: printer.clone(),
        })
    }

    /// Waits, for up to [`OUTPUT_DRAIN`], until the child's output was all read.
    async fn drain_output(&mut self) {
        let _ = tokio::time::timeout(OUTPUT_DRAIN, &mut self.output_closed).await;
    }

    /// Whether the session's program is to pause at its .NET runtime's startup, and nothing has.
    fn no_runtime_paused(&self) -> bool {
        let planned = *self
            .planned
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.debugged
            && planned == Some(Trigger::Runtime)
            && self.held.load(Ordering::Relaxed).not()
    }

    /// Says so once, while the service runs, when its program started `after` ago and none of
    /// its .NET runtimes paused, though one was to. The IDE can then tell the user, instead of
    /// waiting in silence.
    fn say_if_no_runtime_paused_yet(&mut self, after: Duration) {
        let started_long_ago = self
            .started
            .get()
            .is_some_and(|started| started.elapsed() >= after);
        if self.runtime_note == RuntimeNote::Pending && started_long_ago && self.no_runtime_paused()
        {
            self.runtime_note = RuntimeNote::Said;
            self.printer.print(&Event::DebugNoRuntimePaused {
                service: &self.name,
                service_state: ServiceState::Running,
            });
        }
    }

    /// Says so when the session's program was to pause at its .NET runtime's startup, but nothing
    /// ever paused: the IDE then knows to stop waiting, and why nothing was attached.
    ///
    /// A runtime that never connects can't be told apart from one still starting, so this is
    /// final only once the session is gone. [`Self::say_if_no_runtime_paused_yet`] says it
    /// earlier, as a note.
    fn say_if_no_runtime_paused(&self) {
        if self.no_runtime_paused() {
            self.printer.print(&Event::DebugNoRuntimePaused {
                service: &self.name,
                service_state: ServiceState::Exited,
            });
        }
    }

    /// Prints the session's held-back error report as the stderr line it was, when no `exited`
    /// took it: another session ended the run first, or `up` was stopped.
    fn print_unreported_error(&self) {
        let error = self
            .error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(error) = error {
            self.printer.print(&Event::Output {
                service: &self.name,
                source: Source::Session,
                stream: Stream::Stderr,
                line: &session_error_line(&error),
            });
        }
    }

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

    /// Job Object termination reaches every descendant along with the direct
    /// child, matching the Unix forced-shutdown guarantee.
    ///
    /// Only the direct child is waited for, not process-wrap's whole-job `wait`.
    /// That one waits for an event on the job's completion port, and polling
    /// the child's `try_wait` during supervision takes those events off the
    /// port. After a service exits on its own the port can be empty for good,
    /// and the job `wait` would block forever.
    #[cfg(windows)]
    async fn force_stop(&mut self) -> io::Result<()> {
        self.child.start_kill()?;
        self.child.inner_mut().wait().await?;
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

/// The processes `mirrord up` runs for one service.
pub(super) struct ServiceCommands {
    pub(super) name: Arc<str>,
    /// Whether `mirrord up --debug` holds the service's program for a debugger. Once the session
    /// printed its hold line, the debugger owns the program's life: when it ends the program (an
    /// IDE's Stop Debugging does), the run ends without calling it a crash.
    pub(super) debugged: bool,
    /// `run.pre_command`, run to completion without mirrord before `session`.
    pub(super) pre: Option<Command>,
    /// The .NET diagnostic port of a debugged service, whose address `session` carries. Its
    /// runtimes pause on it, and are held until the IDE releases them.
    pub(super) dotnet_port: Option<DiagnosticPort>,
    /// The `mirrord exec`/`mirrord container` child that runs `run.command`.
    pub(super) session: Command,
}

pub(super) async fn run(
    commands: Vec<ServiceCommands>,
    ready: ReadyTracker,
    printer: Printer,
) -> Result<(), UpError> {
    let shutdown = shutdown_signals()?;
    let (result, forced_signal) = supervise_with_second_signal(
        commands,
        ready,
        printer,
        receive_first_signal(shutdown.first),
        receive_second_signal(shutdown.second),
        Timing::default(),
    )
    .await?;
    if let Some(signal) = forced_signal {
        let exit_code = signal.signal.forced_exit_code();
        let _ = signal.accepted.send(());
        std::process::exit(exit_code);
    }
    result
}

/// The waits of supervision, which only tests shorten.
#[derive(Debug, Clone, Copy)]
struct Timing {
    /// How long services get to exit on their own before they're killed.
    grace: Duration,
    /// See [`NO_RUNTIME_PAUSED_AFTER`].
    no_runtime_paused: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            grace: SHUTDOWN_GRACE,
            no_runtime_paused: NO_RUNTIME_PAUSED_AFTER,
        }
    }
}

/// Returns when the first session child exits.
///
/// A pre-command exiting is not such an exit: on success the service's session
/// is spawned in its place, and on failure only that service is dropped, unless
/// it was the last one left.
async fn wait_for_first_exit(
    services: &mut Vec<Service>,
    readiness: &Arc<Readiness>,
    output: &mut JoinSet<()>,
    printer: &Printer,
    no_runtime_paused_after: Duration,
) -> Result<(), UpError> {
    // JSON events stay in order, so an exit is said after the last line the child wrote. Text
    // output reads it only to tell how a session failed.
    let drains_every_exit = printer.format() == OutputFormat::Json;
    loop {
        let mut index = 0;
        while let Some(service) = services.get_mut(index) {
            let Some(status) = service.child.try_wait()? else {
                service.say_if_no_runtime_paused_yet(no_runtime_paused_after);
                index += 1;
                continue;
            };
            let Some(session) = service.then.take() else {
                if status.success().not() || drains_every_exit {
                    service.drain_output().await;
                }
                service.say_if_no_runtime_paused();
                // A hold that failed before announcing never reached a debugger, so its exit
                // still reports as a crash or a failed start.
                let reason = if status.success() {
                    ExitReason::Succeeded
                } else if service.debugged && service.held.load(Ordering::Relaxed) {
                    ExitReason::WhileDebugged
                } else if service.started.get().is_some() {
                    ExitReason::Crashed
                } else {
                    ExitReason::FailedToStart
                };
                let error = service
                    .error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                let name = service.name.clone();
                printer.print(&Event::exited(&name, reason, status, error.as_deref()));
                return match reason {
                    ExitReason::Succeeded | ExitReason::WhileDebugged => Ok(()),
                    ExitReason::Crashed => Err(UpError::ServiceCrashed { name, status }),
                    ExitReason::FailedToStart => Err(UpError::ServiceFailedToStart { name }),
                };
            };
            // Only the leader has exited. Anything it left running in its tree
            // would outlive `mirrord up`, because shutdown only reaches the
            // trees of services still supervised here.
            service.force_stop().await?;
            if drains_every_exit {
                service.drain_output().await;
            }
            if status.success() {
                printer.print(&Event::pre_command(
                    &service.name,
                    PreCommandStage::Succeeded,
                ));
                *service = Service::spawn(
                    service.name.clone(),
                    service.debugged,
                    service.held.clone(),
                    session,
                    None,
                    readiness,
                    output,
                    printer,
                )?;
                index += 1;
            } else {
                let name = services.swap_remove(index).name;
                let remaining = if services.is_empty() {
                    Remaining::NoneLeft
                } else {
                    Remaining::Others
                };
                printer.print(&Event::pre_command_failed(&name, status, remaining));
                if remaining == Remaining::NoneLeft {
                    return Err(UpError::PreCommandFailed { name, status });
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The trigger a `debug-plan` line's JSON names, or `None` for one that doesn't parse.
fn planned_trigger(plan: &str) -> Option<Trigger> {
    let plan = serde_json::from_str::<serde_json::Value>(plan).ok()?;
    match plan.get("trigger")?.as_str()? {
        "runtime" => Some(Trigger::Runtime),
        "process" => Some(Trigger::Process),
        _ => None,
    }
}

/// Prints what a service's diagnostic port reports, prefixed with the service's name like the
/// service's own output, and marks the service `held` once a runtime is.
fn report_runtime_holds(
    name: Arc<str>,
    held: Arc<AtomicBool>,
    printer: Printer,
) -> impl Fn(Report) + Send + Sync + 'static {
    move |report| match report {
        Report::Held(announcement) => {
            held.store(true, Ordering::Relaxed);
            let text = announcement.to_string();
            let hold =
                to_raw_value(&announcement).expect("a hold announcement always serializes to JSON");
            printer.print(&Event::DebugHold {
                service: &name,
                hold: &hold,
                text: &text,
            });
        }
        Report::Ended(ended) => {
            let text = ended.to_string();
            let ended = to_raw_value(&ended).expect("a hold's end always serializes to JSON");
            printer.print(&Event::DebugHoldEnded {
                service: &name,
                ended: &ended,
                text: &text,
            });
        }
        Report::Refused(why) => printer.print(&Event::DebugNote {
            service: &name,
            message: &why,
        }),
        Report::Skipped {
            pid,
            entry_assembly,
        } => tracing::debug!(
            service = %name,
            pid,
            entry_assembly,
            "resumed a paused .NET runtime of the SDK without holding it"
        ),
    }
}

/// The event that passes on `line` of a session's stdout: a progress message, a debug line, or
/// the program's own output.
///
/// A debug line whose JSON isn't an object with a version `v`, as `mirrord_debug_hold` writes every
/// one, is passed on as output, as is any other line.
fn session_line_event<'a>(service: &'a str, line: &'a str) -> Event<'a> {
    let payload = |prefix: &str| {
        let json = line.strip_prefix(prefix)?;
        serde_json::from_str::<Map<String, Value>>(json)
            .ok()?
            .get("v")
            .is_some_and(Value::is_u64)
            .then(|| serde_json::from_str::<&RawValue>(json).ok())
            .flatten()
    };
    if let Some(hold) = payload(ANNOUNCEMENT_PREFIX) {
        Event::DebugHold {
            service,
            hold,
            text: line,
        }
    } else if let Some(ended) = payload(ENDED_PREFIX) {
        Event::DebugHoldEnded {
            service,
            ended,
            text: line,
        }
    } else if let Some(plan) = payload(PLAN_PREFIX) {
        Event::DebugPlan {
            service,
            plan,
            text: line,
        }
    } else if let Some(progress) = is_progress_line(line)
        .then(|| serde_json::from_str::<&RawValue>(line).ok())
        .flatten()
    {
        Event::Progress {
            service,
            progress,
            text: line,
        }
    } else {
        Event::Output {
            service,
            source: Source::Session,
            stream: Stream::Stdout,
            line,
        }
    }
}

/// Passes on each line of `stream` as the service's: in text, prefixed with the service's name.
/// On a session's stdout, also picks up the readiness marker and the branch databases report,
/// which is not printed.
///
/// Under `--output json`, a session's last stderr line is its final error when it's an error
/// report. It's held back, and its report put in [`SessionErrors::error`] at once, so `exited`
/// finds it even while a descendant keeps the pipe open. A next line shows it wasn't the last: it's
/// then printed as output, unless `exited` already took it.
///
/// Lines are decoded lossily: a program may write in another encoding (a Windows console's code
/// page, say), and one bad byte must not stop the forwarding, which would close the pipe on the
/// program's next write.
async fn forward_output(
    stream: impl AsyncRead + Unpin,
    name: Arc<str>,
    printer: Printer,
    forwarded: Forwarded,
) {
    let source = forwarded.source();
    let stream_name = forwarded.stream();
    let output = |line: &str| {
        printer.print(&Event::Output {
            service: &name,
            source,
            stream: stream_name,
            line,
        })
    };
    let error_slot = match &forwarded {
        Forwarded::SessionStderr(errors) if printer.format() == OutputFormat::Json => {
            Some(&errors.error)
        }
        _ => None,
    };
    // The line whose report is in `error_slot`.
    let mut held_back: Option<String> = None;

    let mut reader = BufReader::new(stream);
    let mut buffer = Vec::new();
    let mut counted = false;
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer).await {
            Ok(0) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(
                    buffer
                        .strip_suffix(b"\n")
                        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
                        .unwrap_or(&buffer),
                );
                let Forwarded::SessionStdout(session) = &forwarded else {
                    let Some(slot) = error_slot else {
                        output(&line);
                        continue;
                    };
                    let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    // An empty slot means `exited` took the report as the session's error.
                    if let Some(previous) = held_back.take()
                        && slot.take().is_some()
                    {
                        output(&previous);
                    }
                    match session_error(&line) {
                        Some(error) => {
                            *slot = Some(error);
                            held_back = Some(line.into_owned());
                        }
                        None => output(&line),
                    }
                    continue;
                };
                let ready = counted.not() && is_session_ready_line(&line);
                if ready {
                    counted = true;
                    let _ = session.started.set(Instant::now());
                    session.readiness.mark();
                } else if let Some(report) = line.strip_prefix(UP_DB_BRANCHES_REPORT_PREFIX) {
                    // Only analytics read the report, so one that doesn't parse is dropped
                    // rather than shown.
                    if let Ok(branches) = serde_json::from_str::<Vec<UpSessionDbBranch>>(report) {
                        session
                            .readiness
                            .tracker
                            .record_db_branches(name.clone(), branches);
                    }
                    continue;
                } else if line.starts_with(ANNOUNCEMENT_PREFIX) {
                    // Still printed: the IDE reads the hold line from `mirrord up`'s output.
                    session.held.store(true, Ordering::Relaxed);
                } else if let Some(plan) = line.strip_prefix(PLAN_PREFIX) {
                    *session
                        .planned
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = planned_trigger(plan);
                }
                printer.print(&session_line_event(&name, &line));
                if ready {
                    printer.print(&Event::Ready { service: &name });
                }
            }
            Err(error) => {
                printer.print(&Event::OutputError {
                    service: &name,
                    stream: stream_name,
                    error: error.to_string(),
                });
                break;
            }
        }
    }
}

#[cfg(test)]
async fn supervise(
    commands: Vec<ServiceCommands>,
    ready: ReadyTracker,
    shutdown: impl Future<Output = io::Result<ShutdownSignal>>,
    grace: Duration,
) -> Result<(), UpError> {
    let timing = Timing {
        grace,
        ..Timing::default()
    };
    supervise_with_second_signal(
        commands,
        ready,
        Printer::new(OutputFormat::Text),
        shutdown,
        std::future::pending(),
        timing,
    )
    .await?
    .0
}

async fn supervise_with_second_signal(
    commands: Vec<ServiceCommands>,
    ready: ReadyTracker,
    printer: Printer,
    shutdown: impl Future<Output = io::Result<ShutdownSignal>>,
    second_shutdown: impl Future<Output = io::Result<SignalDelivery>>,
    Timing {
        grace,
        no_runtime_paused,
    }: Timing,
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
    let mut pre_command_spawn_error = None;

    // Each debugged service's diagnostic port, served until supervision ends. Dropping the set
    // closes them, after the services' processes are gone.
    let mut ports = JoinSet::new();
    for ServiceCommands {
        name,
        debugged,
        pre,
        dotnet_port,
        session,
    } in commands
    {
        let held = Arc::new(AtomicBool::new(false));
        if let Some(port) = dotnet_port {
            ports.spawn(port.serve(report_runtime_holds(
                name.clone(),
                held.clone(),
                printer.clone(),
            )));
        }
        let spawned = match pre {
            Some(pre) => {
                printer.print(&Event::pre_command(&name, PreCommandStage::Started));
                // Like a failing pre-command, one that can't start (e.g. its
                // program is not on PATH) only drops its own service.
                match Service::spawn(
                    name.clone(),
                    debugged,
                    held,
                    pre,
                    Some(session),
                    &readiness,
                    &mut output,
                    &printer,
                ) {
                    Err(error) => {
                        printer.print(&Event::pre_command_spawn_failed(&name, &error));
                        pre_command_spawn_error = Some(UpError::PreCommandSpawn {
                            name,
                            source: error,
                        });
                        continue;
                    }
                    spawned => spawned,
                }
            }
            None => Service::spawn(
                name,
                debugged,
                held,
                session,
                None,
                &readiness,
                &mut output,
                &printer,
            ),
        };
        match spawned {
            Ok(service) => services.push(service),
            Err(error) => {
                spawn_error = Some(error);
                break;
            }
        }
    }

    let (result, received_signal) = if let Some(error) = spawn_error {
        // No select will poll this receiver, so dropping it marks any later
        // signal as unaccepted before cleanup begins.
        drop(shutdown);
        (Err(UpError::Io(error)), None)
    } else if let Some(error) = pre_command_spawn_error.filter(|_| services.is_empty()) {
        drop(shutdown);
        (Err(error), None)
    } else {
        {
            let exits = wait_for_first_exit(
                &mut services,
                &readiness,
                &mut output,
                &printer,
                no_runtime_paused,
            );
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
    for service in &services {
        service.print_unreported_error();
    }

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
        printer.print(&Event::StopFailed {
            error: error.to_string(),
        });
    }
    if let Some(error) = second_shutdown_error {
        return Err(UpError::Io(error));
    }
    Ok((result, forced_signal))
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod output_tests {
    use rstest::rstest;
    use tokio::io::AsyncWriteExt;

    use super::*;
    use crate::output::Printed;

    /// A line in another encoding (`£` in a Windows console's code page 850, here) doesn't stop
    /// the forwarding: the readiness marker after it is still read.
    #[tokio::test]
    async fn a_line_that_is_not_utf8_does_not_stop_the_forwarding() {
        let readiness = Arc::new(Readiness {
            count: AtomicUsize::new(0),
            total: 1,
            start: Instant::now(),
            tracker: ReadyTracker::default(),
        });
        let started = Arc::new(OnceLock::new());
        let (closing, _closed) = oneshot::channel();
        let output = [
            b"price: \x9c5\r\n".as_slice(),
            SESSION_READY_MESSAGE.as_bytes(),
            b"\n",
        ]
        .concat();

        forward_output(
            output.as_slice(),
            Arc::from("shop"),
            Printer::new(OutputFormat::Text),
            Forwarded::SessionStdout(SessionOutput {
                readiness: readiness.clone(),
                started: started.clone(),
                held: Arc::new(AtomicBool::new(false)),
                planned: Arc::new(Mutex::new(None)),
                _closing: Arc::new(closing),
            }),
        )
        .await;

        assert!(
            started.get().is_some(),
            "the readiness marker comes after the bad byte, so it must still be read"
        );
        assert_eq!(readiness.count.load(Ordering::Relaxed), 1);
    }

    /// Whether a session's output counted it ready.
    #[derive(Debug, PartialEq, Eq)]
    enum Counted {
        Ready,
        NotReady,
    }

    /// Only a `Ready!` line counts a session ready: `Ready!` alone in `simple` mode, or in `json`
    /// a successful finish whose message is `Ready!`, whatever its task. No other progress
    /// message does.
    #[rstest]
    // `simple` mode prints the running task's finish as its message alone.
    #[case::simple_ready(include_str!("testdata/session-output/simple-ready.txt"), Counted::Ready)]
    // `json` mode finishes the running task with `Ready!`.
    #[case::json_ready(include_str!("testdata/session-output/json-ready.txt"), Counted::Ready)]
    // The running task started, but the layer never reported.
    #[case::json_running_task_alone(
        include_str!("testdata/session-output/json-running-task.txt"),
        Counted::NotReady
    )]
    // A failed finish isn't ready, even with `Ready!` as its message.
    #[case::json_failed_with_ready(
        include_str!("testdata/session-output/json-failed-ready.txt"),
        Counted::NotReady
    )]
    // With `apc` or `iat`, the running task finishes at the handoff, before the layer reports:
    // a successful finish with another message isn't ready.
    #[case::json_handoff_without_ready(
        include_str!("testdata/session-output/json-handoff.txt"),
        Counted::NotReady
    )]
    #[tokio::test]
    async fn only_a_ready_line_counts_as_ready(#[case] output: &str, #[case] expected: Counted) {
        let readiness = Arc::new(Readiness {
            count: AtomicUsize::new(0),
            total: 1,
            start: Instant::now(),
            tracker: ReadyTracker::default(),
        });
        let started = Arc::new(OnceLock::new());
        let (closing, _closed) = oneshot::channel();

        forward_output(
            output.as_bytes(),
            Arc::from("shop"),
            Printer::new(OutputFormat::Text),
            Forwarded::SessionStdout(SessionOutput {
                readiness: readiness.clone(),
                started: started.clone(),
                held: Arc::new(AtomicBool::new(false)),
                planned: Arc::new(Mutex::new(None)),
                _closing: Arc::new(closing),
            }),
        )
        .await;

        let counted = match readiness.count.load(Ordering::Relaxed) {
            0 => Counted::NotReady,
            _ => Counted::Ready,
        };
        assert_eq!(counted, expected, "the session's output was {output:?}");
        assert_eq!(
            started.get().is_some(),
            expected == Counted::Ready,
            "a session got as far as its program exactly when it counted ready"
        );
    }

    /// A session's progress, debug lines, readiness and the program's own lines, from
    /// `session-stdout.txt`.
    const SESSION_STDOUT: &str = include_str!("testdata/output/session-stdout.txt");

    /// Forwards `output` as the stdout of the session of a service called `api`, and returns what
    /// was printed and the tracker of what the session reported.
    async fn forward_session_stdout(
        output: &str,
        format: OutputFormat,
    ) -> (Vec<Printed>, ReadyTracker) {
        let tracker = ReadyTracker::default();
        let readiness = Arc::new(Readiness {
            count: AtomicUsize::new(0),
            total: 1,
            start: Instant::now(),
            tracker: tracker.clone(),
        });
        let (printer, printed) = Printer::captured(format);
        let (closing, _closed) = oneshot::channel();
        forward_output(
            output.as_bytes(),
            Arc::from("api"),
            printer,
            Forwarded::SessionStdout(SessionOutput {
                readiness,
                started: Arc::new(OnceLock::new()),
                held: Arc::new(AtomicBool::new(false)),
                planned: Arc::new(Mutex::new(None)),
                _closing: Arc::new(closing),
            }),
        )
        .await;
        let printed = printed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        (printed, tracker)
    }

    /// Under `--output json`, each line of a session's stdout is its event: progress and debug
    /// lines carry their object as `mirrord exec` printed it, `Ready!` is followed by `ready`,
    /// and the rest, a line that only looks like a debug line or progress included, is output.
    /// The branch databases report is read, not printed.
    #[tokio::test]
    async fn json_turns_each_session_line_into_its_event() {
        let (printed, tracker) = forward_session_stdout(SESSION_STDOUT, OutputFormat::Json).await;

        let lines = printed
            .iter()
            .map(|printed| {
                assert_eq!(printed.stream, Stream::Stdout, "every event goes to stdout");
                printed.line.as_str()
            })
            .collect::<Vec<_>>();
        let expected = include_str!("testdata/output/session-stdout.events.jsonl")
            .lines()
            .collect::<Vec<_>>();
        assert_eq!(
            lines, expected,
            "each session line becomes the event in `session-stdout.events.jsonl`"
        );
        assert_eq!(
            tracker.db_branches().get("api").map(Vec::len),
            Some(1),
            "the report was read"
        );
    }

    /// Without `--output json`, every line of a session's stdout is printed as it was, behind the
    /// service's name, except the branch databases report.
    #[tokio::test]
    async fn text_passes_each_session_line_on_as_it_was() {
        let (printed, _) = forward_session_stdout(SESSION_STDOUT, OutputFormat::Text).await;

        let expected = SESSION_STDOUT
            .lines()
            .filter(|line| line.starts_with(UP_DB_BRANCHES_REPORT_PREFIX).not())
            .map(|line| Printed {
                stream: Stream::Stdout,
                line: format!("api: {line}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            printed, expected,
            "text output passes each line on as it was, behind the service's name"
        );
    }

    /// What forwarding a session's stderr kept as its final error.
    #[derive(Debug, PartialEq, Eq)]
    enum Kept {
        /// The last line, which was an error report, and isn't printed.
        LastLine,
        /// No error: every line was printed.
        Nothing,
    }

    /// Under `--output json`, a session's last stderr line is its final error only if it's an
    /// error report: it then goes on `exited`, not printed. Every other line is printed as
    /// output, a report that isn't last too. Text output keeps nothing back.
    #[rstest]
    // The session's last line is an error report, so it's the session's error.
    #[case::report_last(
        include_str!("testdata/output/stderr-error-last.txt"),
        OutputFormat::Json,
        Kept::LastLine
    )]
    // The program printed a report and went on, so the report isn't the session's error.
    #[case::report_not_last(
        include_str!("testdata/output/stderr-error-not-last.txt"),
        OutputFormat::Json,
        Kept::Nothing
    )]
    // The program's last line is JSON, but no error report.
    #[case::json_that_is_no_report(
        include_str!("testdata/output/stderr-json-not-a-report.txt"),
        OutputFormat::Json,
        Kept::Nothing
    )]
    // `Error: ` and JSON, but no miette report: it has no `message`.
    #[case::report_without_a_message(
        include_str!("testdata/output/stderr-error-without-message.txt"),
        OutputFormat::Json,
        Kept::Nothing
    )]
    // Without the flag no session reports JSON errors, so a report-like line is the program's.
    #[case::text_keeps_nothing(
        include_str!("testdata/output/stderr-error-last.txt"),
        OutputFormat::Text,
        Kept::Nothing
    )]
    #[tokio::test]
    async fn only_a_last_report_line_is_kept_as_the_session_error(
        #[case] stderr: &str,
        #[case] format: OutputFormat,
        #[case] expected: Kept,
    ) {
        let (printer, printed) = Printer::captured(format);
        let error = Arc::new(Mutex::new(None));
        let (closing, _closed) = oneshot::channel();
        forward_output(
            stderr.as_bytes(),
            Arc::from("api"),
            printer,
            Forwarded::SessionStderr(SessionErrors {
                error: error.clone(),
                _closing: Arc::new(closing),
            }),
        )
        .await;

        let lines = stderr.lines().collect::<Vec<_>>();
        let printed_lines = match expected {
            Kept::LastLine => lines.split_last().map(|(_, rest)| rest).unwrap_or_default(),
            Kept::Nothing => lines.as_slice(),
        };
        // JSON lines compare as values, so the test doesn't depend on the order of their keys.
        let expected_printed = printed_lines
            .iter()
            .map(|line| match format {
                OutputFormat::Json => serde_json::json!({
                    "type": "output",
                    "service": "api",
                    "source": "session",
                    "stream": "stderr",
                    "line": line,
                }),
                OutputFormat::Text => serde_json::Value::from(format!("api: {line}")),
            })
            .collect::<Vec<_>>();
        let printed = printed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|printed| match format {
                OutputFormat::Json => {
                    serde_json::from_str(&printed.line).expect("every JSON line parses")
                }
                OutputFormat::Text => serde_json::Value::from(printed.line.clone()),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            printed, expected_printed,
            "every line but a kept report is printed as output"
        );

        let kept = error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        match expected {
            Kept::LastLine => {
                let kept = kept.expect("the last line was a report");
                let last = lines.last().expect("the fixture has lines");
                assert_eq!(
                    Some(kept.get()),
                    last.strip_prefix("Error: "),
                    "the report is kept as it was printed"
                );
            }
            Kept::Nothing => assert!(kept.is_none(), "kept {kept:?}"),
        }
    }

    /// Under `--output json`, a session's error report is its error as soon as it's the last line
    /// so far, so `exited` finds it though a descendant keeps the session's stderr open past the
    /// drain. Once taken, a later line doesn't print it as output too.
    #[tokio::test]
    async fn a_report_is_the_session_error_while_stderr_stays_open() {
        let (printer, printed) = Printer::captured(OutputFormat::Json);
        let error = Arc::new(Mutex::new(None));
        let (closing, _closed) = oneshot::channel();
        let (mut stderr, read_end) = tokio::io::duplex(64 * 1024);
        let forwarding = tokio::spawn(forward_output(
            read_end,
            Arc::from("api"),
            printer,
            Forwarded::SessionStderr(SessionErrors {
                error: error.clone(),
                _closing: Arc::new(closing),
            }),
        ));
        let report = include_str!("testdata/output/ls-error.txt");
        stderr
            .write_all(report.as_bytes())
            .await
            .expect("the forwarding reads the pipe");

        let taken = tokio::time::timeout(test_support::TIMEOUT, async {
            loop {
                let taken = error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                if let Some(taken) = taken {
                    break taken;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the report is the error while the pipe is still open");
        assert_eq!(
            Some(taken.get()),
            report.trim_end().strip_prefix("Error: "),
            "the report is kept as it was printed"
        );

        stderr
            .write_all(b"a descendant's line\n")
            .await
            .expect("the forwarding reads the pipe");
        drop(stderr);
        forwarding.await.expect("the forwarding doesn't panic");
        let lines = printed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|printed| {
                serde_json::from_str::<serde_json::Value>(&printed.line)
                    .expect("every JSON line parses")
                    .get("line")
                    .cloned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            lines,
            [Some(serde_json::Value::from("a descendant's line"))],
            "the report `exited` took isn't output too"
        );
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod event_tests;

#[cfg(all(test, windows))]
mod windows_tests;

#[cfg(all(test, unix))]
mod tests {
    use std::{path::Path, process::Stdio};

    use mirrord_command::resolve_tokio_command;
    use nix::sys::signal::kill;
    use rstest::rstest;
    use tempfile::TempDir;
    use tokio::process::Child;

    use super::{
        test_support::{TEST_GRACE, wait_for_file, wait_for_helper_ready},
        *,
    };

    fn command(script: &str, directory: &Path) -> ServiceCommands {
        let mut command = resolve_tokio_command("sh");
        command
            .args(["-c", script])
            .current_dir(directory)
            .env("READY_MESSAGE", SESSION_READY_MESSAGE)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        ServiceCommands {
            name: Arc::from("test"),
            debugged: false,
            pre: None,
            dotnet_port: None,
            session: command,
        }
    }

    async fn wait_for_process_group_exit(group: Pid) {
        // Killed descendants are adopted by an external reaper, so their group
        // entries can outlast direct-child reaping.
        let exited = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match killpg(group, None) {
                    Ok(()) => tokio::time::sleep(Duration::from_millis(10)).await,
                    Err(Errno::ESRCH) => break,
                    Err(error) => {
                        panic!("failed to inspect service process group {group}: {error}")
                    }
                }
            }
        })
        .await;
        assert!(
            exited.is_ok(),
            "service process group {group} must disappear after forced cleanup"
        );
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
            "trap '' TERM; echo $$ > pid; : > started; exec sleep 60",
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
                Printer::new(OutputFormat::Text),
                first_shutdown,
                second_shutdown,
                Timing {
                    grace: TEST_GRACE,
                    ..Timing::default()
                },
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
        wait_for_process_group_exit(pid).await;
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
            "while [ ! -f started ]; do sleep 0.02; done; echo \"$READY_MESSAGE\"; exit 7",
            directory.path(),
        );
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            supervise(
                vec![sibling, failing],
                ReadyTracker::default(),
                std::future::pending(),
                TEST_GRACE,
            ),
        )
        .await
        .expect("the crash did not end supervision");
        assert!(
            matches!(result, Err(UpError::ServiceCrashed { status, .. }) if status.code() == Some(7))
        );
        assert!(directory.path().join("stopped").exists());
    }

    /// Supervises a sibling next to a debugged service that prints the session-ready line, then
    /// `hold_line` if any, then exits with 7. Returns what supervision returned once the sibling
    /// was stopped.
    async fn supervise_debugged_exit(hold_line: Option<&str>) -> Result<(), UpError> {
        let directory = TempDir::new().unwrap();
        let sibling = command(
            "trap 'touch stopped; exit 0' TERM; touch started; while :; do sleep 0.02; done",
            directory.path(),
        );
        let mut debugged = command(
            "while [ ! -f started ]; do sleep 0.02; done; echo \"$READY_MESSAGE\"; \
             if [ -n \"$HOLD_LINE\" ]; then echo \"$HOLD_LINE\"; fi; exit 7",
            directory.path(),
        );
        debugged.debugged = true;
        if let Some(hold_line) = hold_line {
            debugged.session.env("HOLD_LINE", hold_line);
        }
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            supervise(
                vec![sibling, debugged],
                ReadyTracker::default(),
                std::future::pending(),
                TEST_GRACE,
            ),
        )
        .await
        .expect("the exit did not end supervision");
        assert!(
            directory.path().join("stopped").exists(),
            "any exit stops the siblings"
        );
        result
    }

    /// An IDE's Stop Debugging ends a held program with a non-zero code. That ends the run, and
    /// its siblings, as any exit does, but isn't a crash.
    #[tokio::test]
    async fn debugged_service_exit_stops_siblings_without_a_crash() {
        let result = supervise_debugged_exit(Some(
            r#"debug-hold {"v":2,"trigger":"process","pid":1,"release":"test"}"#,
        ))
        .await;
        assert!(
            result.is_ok(),
            "the program was held, so its exit is the debugger's doing: {result:?}"
        );
    }

    /// A debugged service that never printed its hold line was never held, so no debugger can
    /// have ended it: its exit is still a crash.
    #[tokio::test]
    async fn debugged_service_exit_before_its_hold_is_a_crash() {
        let result = supervise_debugged_exit(None).await;
        assert!(
            matches!(&result, Err(UpError::ServiceCrashed { status, .. }) if status.code() == Some(7)),
            "no hold line was seen, so the exit must report as a crash: {result:?}"
        );
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
        let missing = ServiceCommands {
            name: Arc::from("missing"),
            debugged: false,
            pre: None,
            dotnet_port: None,
            session: resolve_tokio_command(directory.path().join("missing")),
        };
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
                // Ignoring TERM before forking keeps the persistent descendant resistant
                // through exec, so leader-only forced cleanup cannot pass.
                "trap '' TERM; echo $$ > child_pid; (exec sleep 60) & trap ': > first-term' TERM; echo \"$READY_MESSAGE\"; while :; do sleep 0.02; done"
            } else {
                "trap 'touch stopped-int; exit 0' INT; trap 'touch stopped-term; exit 0' TERM; trap 'touch stopped-hup; exit 0' HUP; echo \"$READY_MESSAGE\"; while :; do sleep 0.02; done"
            },
            directory,
        );
        run(
            vec![command],
            ready.clone(),
            Printer::new(OutputFormat::Text),
        )
        .await
        .unwrap();
        assert!(ready.time_to_ready().is_some());
        if std::env::var_os("MIRRORD_UP_SIGNAL_TEST_DRAIN").is_some() {
            std::fs::write(directory.join("draining"), []).unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }

    fn signal_test_helper(
        directory: &Path,
        drain: bool,
        natural_exit: bool,
        ignores_term: bool,
    ) -> Child {
        let mut helper = resolve_tokio_command(std::env::current_exe().unwrap());
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
        wait_for_process_group_exit(child).await;
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
