//! Keeps child ownership alive until shutdown has given services time to exit.
//!
//! A task-driven service runs no session of its own. Its instances, one session per task its
//! watch matched, come and go with the tasks, and none of them ends the run.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    io,
    ops::Not,
    process::ExitStatus,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use futures::{
    FutureExt,
    future::LocalBoxFuture,
    stream::{BoxStream, FuturesUnordered},
};
use mirrord_analytics::{UP_DB_BRANCHES_REPORT_PREFIX, UpSessionDbBranch};
use mirrord_progress::messages::SESSION_READY_MESSAGE;
use mirrord_remote_layer_protocol::LOST_SESSION_EXIT_CODE;
use mirrord_sessions_manager_client::{ReplicaId, SessionsManagerClientError, TaskEvent};
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
use tokio_stream::{StreamExt, StreamMap};
use uuid::Uuid;

use crate::{
    ReadyTracker, UpError,
    owner::{Owner, TaskOwners},
};

mod platform;
use platform::{ShutdownSignal, SignalStreams, receive_signal, signal_streams};

// Container runtimes conventionally allow ten seconds for graceful shutdown.
// Matching that window lets container services clean up before escalation
// kills the runtime client that is responsible for removing the container.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// How long an exited session's stdout is still read before telling whether it got as far as
/// [`SESSION_READY_MESSAGE`]. The exit can be seen before the forwarding task has read the last
/// lines, and a descendant still holding the pipe open must not hold up the error.
const STDOUT_DRAIN: Duration = Duration::from_secs(1);

/// How the line a held program's `mirrord exec` prints starts: `debug-hold pid=<pid>
/// release=<release>`. `mirrord-layer-lib`'s debug hold prints it once the program is held and
/// an IDE may attach.
const DEBUG_HOLD_LINE_PREFIX: &str = "debug-hold ";

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
    /// Set once the session printed [`SESSION_READY_MESSAGE`], i.e. it got as far as launching
    /// the service's process. Tells a session that failed to start from a service that crashed.
    started: Arc<AtomicBool>,
    /// Set once the session printed its [`DEBUG_HOLD_LINE_PREFIX`] line, i.e. the program is held
    /// and an IDE may attach. From then on a debugged program's exit is the debugger's doing.
    held: Arc<AtomicBool>,
    /// Resolves once the forwarding of the session's stdout ended, so `started` and `held` are
    /// final.
    stdout_closed: Option<oneshot::Receiver<()>>,
}

/// What forwarding a child's stdout watches for.
#[derive(Clone, Copy)]
enum StdoutWatch<'a> {
    /// Nothing: a pre-command.
    Nothing,
    /// The lines of a service's session, whose readiness marker counts toward the run's
    /// readiness.
    Session(&'a Arc<Readiness>),
    /// The lines of an instance of a task-driven service. Its readiness marker counts for the
    /// instance only: it tells the exit of the developer's program from the session failing
    /// before it ran it.
    Instance,
}

/// What forwarding a session's stdout (not a pre-command's) reports back.
struct SessionOutput {
    /// [`None`] for an instance of a task-driven service, which doesn't count toward the run's
    /// readiness.
    readiness: Option<Arc<Readiness>>,
    started: Arc<AtomicBool>,
    held: Arc<AtomicBool>,
    /// Dropped when forwarding ends, which resolves [`Service::stdout_closed`].
    _closed: oneshot::Sender<()>,
}

impl Service {
    /// Spawns `command` in its own process tree and forwards its output.
    ///
    /// `watch` says what the child's stdout is read for.
    fn spawn(
        name: Arc<str>,
        debugged: bool,
        command: Command,
        then: Option<Command>,
        watch: StdoutWatch<'_>,
        output: &mut JoinSet<()>,
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
        let started = Arc::new(AtomicBool::new(false));
        let held = Arc::new(AtomicBool::new(false));
        let mut stdout_closed = None;
        if let Some(stdout) = child.stdout().take() {
            let readiness = match watch {
                StdoutWatch::Nothing => None,
                StdoutWatch::Session(readiness) => Some(Some(readiness.clone())),
                StdoutWatch::Instance => Some(None),
            };
            let session = readiness.map(|readiness| {
                let (closed, receiver) = oneshot::channel();
                stdout_closed = Some(receiver);
                SessionOutput {
                    readiness,
                    started: started.clone(),
                    held: held.clone(),
                    _closed: closed,
                }
            });
            output.spawn(forward_output(stdout, name.clone(), session));
        }
        if let Some(stderr) = child.stderr().take() {
            output.spawn(forward_output(stderr, name.clone(), None));
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
            stdout_closed,
        })
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
    /// The `mirrord exec`/`mirrord container` child that runs `run.command`.
    pub(super) session: Command,
}

/// The processes `mirrord up` runs for a task-driven service.
pub(super) struct TaskServiceCommands {
    pub(super) name: Arc<str>,
    /// `run.pre_command`, run to completion once, before any instance starts.
    pub(super) pre: Option<Command>,
    /// Builds the `mirrord exec` child of the instance for the task on a replica.
    pub(super) instance: InstanceCommand,
    /// Set for a service with `replace_task`: how it reaches the tasks it replaces.
    pub(super) replace: Option<TaskOwners>,
    /// What the service's watch reports. Ends only after an error.
    pub(super) tasks: BoxStream<'static, Result<TaskEvent, SessionsManagerClientError>>,
}

/// Builds the session of a task-driven service's instance, for the task on the replica.
pub(super) type InstanceCommand = Box<dyn Fn(&ReplicaId) -> Result<Command, UpError>>;

/// A task, as its watch names it: the agent instance on a replica.
type TaskKey = (ReplicaId, Uuid);

/// The instances of ended tasks while they are stopped, and the trees of instances that exited
/// while they are killed, each with its name and how its stop went. Also the owner connections of
/// the tasks of a service whose `run.pre_command` failed, while they end the tasks as lost.
///
/// Stopped apart from the supervision loop, which would otherwise wait out each one's grace
/// period before it reads the next event. Each future owns its instance, so dropping one kills
/// the instance's tree at once (`KillOnDrop`).
type Stopping = FuturesUnordered<LocalBoxFuture<'static, (Arc<str>, io::Result<()>)>>;

/// Kills what is left of the tree of `instance`, whose leader exited, as a future of
/// `stopping`. A failure is reported and leaves the run going.
fn force_stop_instance(stopping: &mut Stopping, mut instance: Service) {
    stopping.push(
        async move {
            let result = instance.force_stop().await;
            (instance.name.clone(), result)
        }
        .boxed_local(),
    );
}

/// Waits, as a future of `stopping`, for `owners` to end their tasks with the code they were
/// given, bounded by `grace` as on teardown.
fn wait_for_owners(stopping: &mut Stopping, name: Arc<str>, owners: Vec<Owner>, grace: Duration) {
    if owners.is_empty() {
        return;
    }
    stopping.push(
        async move {
            let _ = tokio::time::timeout(
                grace,
                futures::future::join_all(owners.into_iter().map(Owner::closed)),
            )
            .await;
            (name, Ok(()))
        }
        .boxed_local(),
    );
}

/// A running instance of a task-driven service.
struct Instance {
    task: TaskKey,
    service: Service,
    /// The exit status of the instance's leader, and when it was seen, while its stdout is still
    /// read for the readiness marker.
    exited: Option<(ExitStatus, Instant)>,
}

impl Instance {
    /// The code the instance's task ends with, once the instance's leader exited. [`None`] while
    /// it runs, and while it's unknown whether it ran the developer's program.
    ///
    /// The readiness marker says the session got as far as running the developer's program, so
    /// the exit is that program's. Without it, the session failed before, and the task ends as
    /// lost rather than with mirrord's own exit code. The exit can be seen before the forwarding
    /// of stdout read the marker, so that is waited for, up to [`STDOUT_DRAIN`]. Never waits
    /// itself: the code is decided in one go, so that a run that ends meanwhile can't lose it.
    fn exit_code(&mut self) -> io::Result<Option<i32>> {
        let (status, exited_at) = match self.exited {
            Some(exited) => exited,
            None => {
                let Some(status) = self.service.child.try_wait()? else {
                    return Ok(None);
                };
                *self.exited.insert((status, Instant::now()))
            }
        };
        let name = &self.service.name;
        if self.service.started.load(Ordering::Relaxed) {
            println!("{name}: exited with {status}");
            return Ok(Some(replaced_task_exit_code(status)));
        }
        let drained = self.service.stdout_closed.as_mut().is_none_or(|closed| {
            matches!(closed.try_recv(), Err(oneshot::error::TryRecvError::Empty)).not()
        });
        if drained.not() && exited_at.elapsed() < STDOUT_DRAIN {
            return Ok(None);
        }
        if self.service.started.load(Ordering::Relaxed) {
            println!("{name}: exited with {status}");
            Ok(Some(replaced_task_exit_code(status)))
        } else {
            eprintln!("{name}: exited with {status} before it ran the local process");
            Ok(Some(LOST_SESSION_EXIT_CODE.into()))
        }
    }
}

/// Stops `instance` with `grace`, as a future of `stopping`.
fn stop_instance(stopping: &mut Stopping, mut instance: Service, grace: Duration) {
    stopping.push(
        async move {
            let result = instance.stop(None, grace).await;
            (instance.name.clone(), result)
        }
        .boxed_local(),
    );
}

/// A task-driven service while it is supervised.
struct TaskService {
    name: Arc<str>,
    instance: InstanceCommand,
    phase: TaskPhase,
    /// Every task an instance was started or queued for, also after the instance exited, so
    /// that a task the watch reports again starts no second instance.
    seen: HashSet<TaskKey>,
    /// The running instances.
    instances: Vec<Instance>,
    /// Set for a service with `replace_task`: how it reaches the tasks it replaces.
    replace: Option<TaskOwners>,
    /// The owner connection of each replaced task (see [`crate::owner`]), from the match until it
    /// sent the exit code. A task whose connection is still opening has no instance yet.
    owners: HashMap<TaskKey, Owner>,
}

enum TaskPhase {
    /// `run.pre_command` runs. The tasks matched meanwhile wait for it in `pending`.
    Preparing {
        pre: Box<Service>,
        pending: Vec<TaskKey>,
    },
    /// Every matched task gets an instance.
    Running,
    /// `run.pre_command` failed, so the service starts nothing.
    Dropped,
}

impl TaskService {
    /// Starts the service, or its `run.pre_command`. Fails when the pre-command can't be
    /// started, which only drops this service, as for other services.
    fn start(
        name: Arc<str>,
        pre: Option<Command>,
        instance: InstanceCommand,
        replace: Option<TaskOwners>,
        readiness: &Arc<Readiness>,
        output: &mut JoinSet<()>,
    ) -> Result<Self, UpError> {
        let mut service = Self {
            name: name.clone(),
            instance,
            phase: TaskPhase::Running,
            seen: HashSet::new(),
            instances: Vec::new(),
            replace,
            owners: HashMap::new(),
        };
        let Some(pre) = pre else {
            service.run(readiness);
            return Ok(service);
        };
        println!("{name}: running `run.pre_command`");
        match Service::spawn(name.clone(), false, pre, None, StdoutWatch::Nothing, output) {
            Ok(pre) => {
                service.phase = TaskPhase::Preparing {
                    pre: Box::new(pre),
                    pending: Vec::new(),
                };
                Ok(service)
            }
            Err(source) => {
                eprintln!("{name}: `run.pre_command` failed to start: {source}");
                Err(UpError::PreCommandSpawn { name, source })
            }
        }
    }

    /// Lets every matched task get an instance. The service counts as ready from here on, as its
    /// watch is open.
    fn run(&mut self, readiness: &Arc<Readiness>) {
        self.phase = TaskPhase::Running;
        readiness.mark();
        let replace = if self.replace.is_some() {
            " A task is replaced only when it sets `MIRRORD_REMOTE_ADMISSION`."
        } else {
            ""
        };
        println!(
            "{}: waiting for tasks whose AWS Batch job tags match `target.tag_filter` and \
             `target.jq_filter`. A task can match only when its mirrord companion is new enough \
             to send its job ID to the operator.{replace}",
            self.name
        );
    }

    fn is_dropped(&self) -> bool {
        matches!(self.phase, TaskPhase::Dropped)
    }

    /// Starts an instance for a task the watch matched, unless one was started or queued for it
    /// before.
    ///
    /// A replaced task gets its owner connection first, also while `run.pre_command` runs, so
    /// that the task isn't lost meanwhile. Its instance starts in [`Self::reap_instances`], once
    /// the connection is open and the service runs.
    ///
    /// `replace` is whether the sessions-manager replaced the task for the watch. A service with
    /// `replace_task` starts nothing for a task that wasn't: the task runs its own code.
    fn matched(&mut self, task: TaskKey, replace: bool, output: &mut JoinSet<()>) {
        if self.seen.insert(task.clone()).not() || self.is_dropped() {
            return;
        }
        if let Some(owners) = &self.replace {
            let name = self.instance_name(&task.0);
            if replace.not() {
                eprintln!(
                    "{name}: a task matched, but the operator didn't replace it, so nothing starts \
                     for it. Replacing tasks needs an operator that supports `replace_task`."
                );
                return;
            }
            let owner = owners.open(&task.0, name);
            self.owners.insert(task, owner);
            return;
        }
        match &mut self.phase {
            TaskPhase::Preparing { pending, .. } => pending.push(task),
            TaskPhase::Running => self.spawn_instance(task, output),
            TaskPhase::Dropped => {}
        }
    }

    /// Forgets a task that ended, so that the watch can match it again, should it come back.
    /// Returns its instance, for the caller to stop.
    ///
    /// A replaced task's owner connection is dropped without a code: the task is gone, or was
    /// withdrawn and runs its own code. The instances being stopped for ended tasks therefore
    /// have no owner left to end as lost on teardown.
    fn ended(&mut self, task: &TaskKey) -> Option<Service> {
        self.seen.remove(task);
        if let Some(owner) = self.owners.remove(task) {
            owner.abort();
        }
        if let TaskPhase::Preparing { pending, .. } = &mut self.phase {
            pending.retain(|pending| pending != task);
        }
        let position = self
            .instances
            .iter()
            .position(|instance| &instance.task == task)?;
        let instance = self.instances.swap_remove(position).service;
        println!("{}: the task ended, stopping", instance.name);
        Some(instance)
    }

    /// Starts the instance for `task`. A failure is reported and leaves the other instances
    /// running.
    fn spawn_instance(&mut self, task: TaskKey, output: &mut JoinSet<()>) {
        let name = self.instance_name(&task.0);
        println!("{name}: a task matched, starting");
        let spawned = (self.instance)(&task.0).and_then(|command| {
            Service::spawn(
                name.clone(),
                false,
                command,
                None,
                StdoutWatch::Instance,
                output,
            )
            .map_err(UpError::Io)
        });
        match spawned {
            Ok(service) => self.instances.push(Instance {
                task,
                service,
                exited: None,
            }),
            Err(error) => {
                eprintln!("{name}: failed to start: {error}");
                if let Some(owner) = self.owners.get_mut(&task) {
                    owner.report_lost();
                }
            }
        }
    }

    /// The name of the instance for the task on `replica`, which prefixes its output.
    fn instance_name(&self, replica: &ReplicaId) -> Arc<str> {
        format!("{}/{}", self.name, short_replica(replica)).into()
    }

    /// Starts the instances of the replaced tasks whose owner connections opened, and reaps the
    /// instances that exited, reporting their exit codes to replaced tasks. Their exit doesn't
    /// end the run, and neither does a failure to reap one: its task ends as lost.
    ///
    /// Never waits, so that a run that ends meanwhile can't lose a code. What is left of an
    /// exited instance's tree is killed in `stopping`.
    fn reap_instances(&mut self, output: &mut JoinSet<()>, stopping: &mut Stopping) {
        if matches!(self.phase, TaskPhase::Running) {
            let opened: Vec<_> = self
                .owners
                .iter_mut()
                .filter_map(|(task, owner)| Some((task.clone(), owner.opened()?)))
                .collect();
            for (task, opened) in opened {
                if opened {
                    self.spawn_instance(task, output);
                } else {
                    self.owners.remove(&task);
                }
            }
        }
        self.owners.retain(|_, owner| owner.is_done().not());

        let mut index = 0;
        while let Some(instance) = self.instances.get_mut(index) {
            let code = match instance.exit_code() {
                Ok(Some(code)) => code,
                Ok(None) => {
                    index += 1;
                    continue;
                }
                Err(error) => {
                    eprintln!(
                        "{}: failed to check whether it exited, stopping: {error}",
                        instance.service.name
                    );
                    LOST_SESSION_EXIT_CODE.into()
                }
            };
            let Instance { task, service, .. } = self.instances.swap_remove(index);
            if let Some(owner) = self.owners.get_mut(&task) {
                owner.report_exit(code);
            }
            // Only the leader has exited, as with a pre-command.
            force_stop_instance(stopping, service);
        }
    }

    /// Ends every task the service replaces as lost, rather than after the companion's 30
    /// seconds without connections, and hands over their owner connections to wait for. For a
    /// run that is ending, before its instances are stopped, and for a service that won't
    /// start.
    ///
    /// An instance that exited after it ran the developer's program, while its stdout was still
    /// read, ends its task with its own code.
    fn lose_tasks(&mut self) -> Vec<Owner> {
        for instance in &self.instances {
            let Some((status, _)) = instance.exited else {
                continue;
            };
            if instance.service.started.load(Ordering::Relaxed)
                && let Some(owner) = self.owners.get_mut(&instance.task)
            {
                owner.report_exit(replaced_task_exit_code(status));
            }
        }
        self.owners
            .drain()
            .map(|(_, mut owner)| {
                owner.report_lost();
                owner
            })
            .collect()
    }

    /// Every process of the service that is still supervised.
    fn processes(&mut self) -> impl Iterator<Item = &mut Service> {
        let pre = match &mut self.phase {
            TaskPhase::Preparing { pre, .. } => Some(pre.as_mut()),
            TaskPhase::Running | TaskPhase::Dropped => None,
        };
        pre.into_iter().chain(
            self.instances
                .iter_mut()
                .map(|instance| &mut instance.service),
        )
    }
}

/// The exit code a replaced task ends with for its instance's exit `status`: the instance's code,
/// or `128 + <signal>` when a signal killed it.
fn replaced_task_exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return 128 + signal;
    }
    LOST_SESSION_EXIT_CODE.into()
}

/// A few characters that tell a task's instance apart in the output: the start of the last
/// segment of its replica id (an ECS task ARN ends in the task id).
fn short_replica(replica: &ReplicaId) -> String {
    let id = replica
        .as_str()
        .rsplit(['/', ':'])
        .next()
        .unwrap_or_default();
    id.chars().take(8).collect()
}

/// Every process of `services` and `tasks`, for teardown.
fn all_processes<'a>(
    services: &'a mut [Service],
    tasks: &'a mut [TaskService],
) -> impl Iterator<Item = &'a mut Service> {
    services
        .iter_mut()
        .chain(tasks.iter_mut().flat_map(TaskService::processes))
}

pub(super) async fn run(
    commands: Vec<ServiceCommands>,
    task_commands: Vec<TaskServiceCommands>,
    ready: ReadyTracker,
) -> Result<(), UpError> {
    let shutdown = shutdown_signals()?;
    let (result, forced_signal) = supervise_with_second_signal(
        commands,
        task_commands,
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

/// Returns when the first session child of a service that isn't task-driven exits, or when the
/// watch of a task-driven service fails.
///
/// A pre-command exiting is not such an exit: on success the service's session
/// is spawned in its place, and on failure only that service is dropped, unless
/// it was the last one left. Neither is an instance of a task-driven service exiting, so a run
/// with only task-driven services lasts until it is stopped.
async fn wait_for_first_exit(
    services: &mut Vec<Service>,
    tasks: &mut [TaskService],
    watches: &mut StreamMap<
        usize,
        BoxStream<'static, Result<TaskEvent, SessionsManagerClientError>>,
    >,
    stopping: &mut Stopping,
    readiness: &Arc<Readiness>,
    output: &mut JoinSet<()>,
    grace: Duration,
) -> Result<(), UpError> {
    loop {
        // Instances come and go for as long as the run lasts, so the forwarding of their output
        // is reaped as it ends.
        while output.try_join_next().is_some() {}

        let mut index = 0;
        while let Some(service) = services.get_mut(index) {
            let Some(status) = service.child.try_wait()? else {
                index += 1;
                continue;
            };
            let Some(session) = service.then.take() else {
                if status.success() {
                    return Ok(());
                }
                if let Some(closed) = service.stdout_closed.take() {
                    let _ = tokio::time::timeout(STDOUT_DRAIN, closed).await;
                }
                let name = service.name.clone();
                let started = service.started.load(Ordering::Relaxed);
                // A hold that failed before announcing never reached a debugger, so its exit
                // still reports as a crash or a failed start.
                if service.debugged && service.held.load(Ordering::Relaxed) {
                    println!("{name}: exited with {status} while debugged, stopping");
                    return Ok(());
                }
                return Err(if started {
                    UpError::ServiceCrashed { name, status }
                } else {
                    UpError::ServiceFailedToStart { name }
                });
            };
            // Only the leader has exited. Anything it left running in its tree
            // would outlive `mirrord up`, because shutdown only reaches the
            // trees of services still supervised here.
            service.force_stop().await?;
            if status.success() {
                println!("{}: `run.pre_command` succeeded, starting", service.name);
                *service = Service::spawn(
                    service.name.clone(),
                    service.debugged,
                    session,
                    None,
                    StdoutWatch::Session(readiness),
                    output,
                )?;
                index += 1;
            } else {
                let name = services.swap_remove(index).name;
                if services.is_empty() && tasks.iter().all(TaskService::is_dropped) {
                    return Err(UpError::PreCommandFailed { name, status });
                }
                eprintln!("{name}: `run.pre_command` failed with {status}, not starting");
            }
        }

        for index in 0..tasks.len() {
            let Some(task) = tasks.get_mut(index) else {
                break;
            };
            task.reap_instances(output, stopping);
            let TaskPhase::Preparing { pre, .. } = &mut task.phase else {
                continue;
            };
            let Some(status) = pre.child.try_wait()? else {
                continue;
            };
            pre.force_stop().await?;
            let TaskPhase::Preparing { pending, .. } =
                std::mem::replace(&mut task.phase, TaskPhase::Dropped)
            else {
                unreachable!("the phase was matched above");
            };
            let name = task.name.clone();
            if status.success() {
                println!("{name}: `run.pre_command` succeeded, starting");
                task.run(readiness);
                for pending in pending {
                    task.spawn_instance(pending, output);
                }
                continue;
            }
            watches.remove(&index);
            // Nothing will start for the tasks it held, so they end as lost.
            wait_for_owners(stopping, name.clone(), task.lose_tasks(), grace);
            if services.is_empty() && tasks.iter().all(TaskService::is_dropped) {
                return Err(UpError::PreCommandFailed { name, status });
            }
            eprintln!("{name}: `run.pre_command` failed with {status}, not starting");
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            Some((name, result)) = stopping.next() => {
                if let Err(error) = result {
                    eprintln!("{name}: failed to stop: {error}");
                }
            }
            Some((index, event)) = watches.next() => {
                let Some(task) = tasks.get_mut(index) else {
                    continue;
                };
                match event {
                    Ok(TaskEvent::Matched(task_match)) => task.matched(
                        (task_match.replica_id, task_match.agent_instance_id),
                        task_match.replace,
                        output,
                    ),
                    Ok(TaskEvent::Ended(ended)) => {
                        if let Some(instance) =
                            task.ended(&(ended.replica_id, ended.agent_instance_id))
                        {
                            stop_instance(stopping, instance, grace);
                        }
                    }
                    Err(source) => {
                        return Err(UpError::TaskWatch {
                            name: task.name.clone(),
                            source: Box::new(source),
                        });
                    }
                }
            }
        }
    }
}

/// Prints each line of `stream` prefixed with the service's name. On a session's stdout, also
/// picks up the readiness marker and the branch databases report, which is not printed.
///
/// Lines are decoded lossily: a program may write in another encoding (a Windows console's code
/// page, say), and one bad byte must not stop the forwarding, which would close the pipe on the
/// program's next write.
async fn forward_output(
    stream: impl AsyncRead + Unpin,
    name: Arc<str>,
    session: Option<SessionOutput>,
) {
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
                if let Some(session) = &session {
                    if counted.not() && line.trim() == SESSION_READY_MESSAGE {
                        counted = true;
                        session.started.store(true, Ordering::Relaxed);
                        if let Some(readiness) = &session.readiness {
                            readiness.mark();
                        }
                    } else if let Some(report) = line.strip_prefix(UP_DB_BRANCHES_REPORT_PREFIX) {
                        // Only analytics read the report, so one that doesn't parse is dropped
                        // rather than shown.
                        if let (Some(readiness), Ok(branches)) = (
                            &session.readiness,
                            serde_json::from_str::<Vec<UpSessionDbBranch>>(report),
                        ) {
                            readiness.tracker.record_db_branches(name.clone(), branches);
                        }
                        continue;
                    } else if line.starts_with(DEBUG_HOLD_LINE_PREFIX) {
                        // Still printed: the IDE reads the hold line from `mirrord up`'s output.
                        session.held.store(true, Ordering::Relaxed);
                    }
                }
                println!("{name}: {line}");
            }
            Err(error) => {
                eprintln!("{name} output error: {error}");
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
    supervise_with_second_signal(
        commands,
        Vec::new(),
        ready,
        shutdown,
        std::future::pending(),
        grace,
    )
    .await?
    .0
}

async fn supervise_with_second_signal(
    commands: Vec<ServiceCommands>,
    task_commands: Vec<TaskServiceCommands>,
    ready: ReadyTracker,
    shutdown: impl Future<Output = io::Result<ShutdownSignal>>,
    second_shutdown: impl Future<Output = io::Result<SignalDelivery>>,
    grace: Duration,
) -> Result<(Result<(), UpError>, Option<SignalDelivery>), UpError> {
    let total = commands.len() + task_commands.len();
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

    for ServiceCommands {
        name,
        debugged,
        pre,
        session,
    } in commands
    {
        let spawned = match pre {
            Some(pre) => {
                println!("{name}: running `run.pre_command`");
                // Like a failing pre-command, one that can't start (e.g. its
                // program is not on PATH) only drops its own service.
                match Service::spawn(
                    name.clone(),
                    debugged,
                    pre,
                    Some(session),
                    StdoutWatch::Nothing,
                    &mut output,
                ) {
                    Err(error) => {
                        eprintln!("{name}: `run.pre_command` failed to start: {error}");
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
                session,
                None,
                StdoutWatch::Session(&readiness),
                &mut output,
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

    let mut tasks = Vec::with_capacity(task_commands.len());
    let mut watches = StreamMap::new();
    let mut stopping = Stopping::new();
    if spawn_error.is_none() {
        for TaskServiceCommands {
            name,
            pre,
            instance,
            replace,
            tasks: watch,
        } in task_commands
        {
            match TaskService::start(name, pre, instance, replace, &readiness, &mut output) {
                Ok(task) => {
                    watches.insert(tasks.len(), watch);
                    tasks.push(task);
                }
                Err(error) => pre_command_spawn_error = Some(error),
            }
        }
    }

    let (result, received_signal) = if let Some(error) = spawn_error {
        // No select will poll this receiver, so dropping it marks any later
        // signal as unaccepted before cleanup begins.
        drop(shutdown);
        (Err(UpError::Io(error)), None)
    } else if let Some(error) =
        pre_command_spawn_error.filter(|_| services.is_empty() && tasks.is_empty())
    {
        drop(shutdown);
        (Err(error), None)
    } else {
        {
            let exits = wait_for_first_exit(
                &mut services,
                &mut tasks,
                &mut watches,
                &mut stopping,
                &readiness,
                &mut output,
                grace,
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
    // The watches go first, so that no task is matched to a run that is ending.
    drop(watches);
    // The replaced tasks end as lost before their instances are stopped, at once rather than
    // after the companion's 30 seconds without connections.
    let owners: Vec<_> = tasks.iter_mut().flat_map(TaskService::lose_tasks).collect();
    let cleanup = Box::pin(async {
        let (stopped, ended, _) = futures::future::join3(
            futures::future::join_all(
                all_processes(&mut services, &mut tasks)
                    .map(|service| service.stop(received_signal, grace)),
            ),
            // The instances of ended tasks keep stopping as they were.
            (&mut stopping)
                .map(|(_, result)| result)
                .collect::<Vec<_>>(),
            tokio::time::timeout(
                grace,
                futures::future::join_all(owners.into_iter().map(Owner::closed)),
            ),
        )
        .await;
        (stopped, ended)
    });
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
        Some((mut cleanup, stopped)) => {
            cleanup.extend(stopped);
            cleanup
        }
        None => {
            // Kills the instances of ended tasks still stopping.
            drop(stopping);
            futures::future::join_all(
                all_processes(&mut services, &mut tasks).map(Service::force_stop),
            )
            .await
        }
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

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod output_tests {
    use super::*;

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
        let started = Arc::new(AtomicBool::new(false));
        let (closed, _receiver) = oneshot::channel();
        let output = [
            b"price: \x9c5\r\n".as_slice(),
            SESSION_READY_MESSAGE.as_bytes(),
            b"\n",
        ]
        .concat();

        forward_output(
            output.as_slice(),
            Arc::from("shop"),
            Some(SessionOutput {
                readiness: Some(readiness.clone()),
                started: started.clone(),
                held: Arc::new(AtomicBool::new(false)),
                _closed: closed,
            }),
        )
        .await;

        assert!(
            started.load(Ordering::Relaxed),
            "the readiness marker comes after the bad byte, so it must still be read"
        );
        assert_eq!(readiness.count.load(Ordering::Relaxed), 1);
    }
}

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
                Vec::new(),
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
        let result = supervise_debugged_exit(Some("debug-hold pid=1 release=test")).await;
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
        run(vec![command], Vec::new(), ready.clone()).await.unwrap();
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
