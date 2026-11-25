//! The owner connection of a replaced task.
//!
//! A task that the watch of a `replace_task` service matched is held before its application
//! starts, until a developer's local process ends it with an exit code. The instance's own session
//! can't send that code: `mirrord exec` replaces itself with the local process and never sees its
//! exit status. So `mirrord up` keeps a mirrord-protocol connection of its own to each such task,
//! the owner connection, from before the instance starts until it exits, and sends the code on it
//! with [`ClientMessage::ReplacedTaskExit`]. The code travels on the same ordered stream as the
//! connection's end, so the two can't race. The companion confirms the code by answering the
//! [`ClientMessage::Ping`] sent after it, and until it does, the code is sent again on new
//! connections.
//!
//! The workload companion ends a held task with [`LOST_SESSION_EXIT_CODE`] once no owner
//! connection has been open for 30 seconds. Every owner connection announces itself with
//! [`ClientMessage::ReplacedTaskOwner`] as soon as it is open, and the companion counts only
//! those, so that the instance's own connections can't keep the task held once `mirrord up` is
//! gone, whatever way it ended. The owner connection keeps the task held while the instance runs,
//! and is opened again when it drops.

use std::{ops::Not, sync::Arc, time::Duration};

use futures::{SinkExt, StreamExt};
use mirrord_operator_websocket::connection::{OperatorConnection, OperatorConnectionError};
use mirrord_protocol::{ClientMessage, DaemonMessage, codec::REPLACED_TASK_EXIT_VERSION};
use mirrord_remote_layer_protocol::LOST_SESSION_EXIT_CODE;
use mirrord_sessions_manager_client::{
    IntproxyClient, OperatorTransport, ReplicaId, ServiceScope, SessionsManagerClientError,
    SessionsManagerConnectInfo,
};
use thiserror::Error;
use tokio::{
    sync::oneshot::{self, error::TryRecvError},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};

/// How long an owner connection is tried when it's first opened, and how long an exit code is
/// tried to be delivered: as long as the companion keeps a held task without connections.
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);

/// The pause between two attempts to open an owner connection, and the first pause while one
/// is reopened.
const OPEN_RETRY_DELAY: Duration = Duration::from_secs(1);

/// The longest pause between two attempts to reopen an owner connection while the instance runs.
const REOPEN_MAX_DELAY: Duration = Duration::from_secs(10);

/// How long the companion has to confirm an exit code, by answering the [`ClientMessage::Ping`]
/// sent after it, or by closing the connection.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

/// How `mirrord up` reaches the tasks that the watch of a `replace_task` service matched: through
/// the operator-hosted sessions-manager, under the session of the watch, which the
/// sessions-manager holds the tasks for.
#[derive(Clone)]
pub struct TaskOwners {
    transport: OperatorTransport,
    scope: ServiceScope,
    user_session_id: String,
    ping: Duration,
}

impl TaskOwners {
    /// `user_session_id` is the session of the service's watch. `ping` is how often an owner
    /// connection pings its task, the interval of the instances' own intproxies.
    pub fn new(
        transport: OperatorTransport,
        scope: ServiceScope,
        user_session_id: String,
        ping: Duration,
    ) -> Self {
        Self {
            transport,
            scope,
            user_session_id,
            ping,
        }
    }

    /// Starts opening the owner connection of the task on `replica`, pinned to that replica as
    /// the task's instance is. `name` is the instance's name, which prefixes what the connection
    /// prints.
    pub(crate) fn open(&self, replica: &ReplicaId, name: Arc<str>) -> Owner {
        let connect_info = SessionsManagerConnectInfo {
            scope: self.scope.clone(),
            user_session_id: self.user_session_id.clone(),
            replica_filter: None,
            agent_selector: None,
            assigned_replica: Some(replica.clone()),
        };
        let (opened_tx, opened_rx) = oneshot::channel();
        let (exit_tx, exit_rx) = oneshot::channel();
        let task = tokio::spawn(own(
            connect_info,
            self.transport.clone(),
            name,
            self.ping,
            opened_tx,
            exit_rx,
        ));

        Owner {
            opened: Some(opened_rx),
            exit: Some(exit_tx),
            task,
        }
    }
}

/// The owner connection of one replaced task, as the supervision of its service sees it.
pub(crate) struct Owner {
    /// owner connection --the connection is open, and the companion takes the exit code-->
    /// supervision. [`None`] once read.
    opened: Option<oneshot::Receiver<()>>,
    /// supervision --the exit code of the task's instance--> owner connection. [`None`] once
    /// sent. Dropped unsent, it ends the connection without a code.
    exit: Option<oneshot::Sender<i32>>,
    task: JoinHandle<()>,
}

impl Owner {
    /// Whether the connection opened: `Some(true)` once it did, `Some(false)` once it couldn't,
    /// and [`None`] while it is opening and after either was returned.
    pub(crate) fn opened(&mut self) -> Option<bool> {
        let opened = match self.opened.as_mut()?.try_recv() {
            Ok(()) => true,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Closed) => false,
        };
        self.opened = None;
        Some(opened)
    }

    /// Ends the task with `code`, unless a code was sent already. A connection that is still
    /// opening sends it once open.
    pub(crate) fn report_exit(&mut self, code: i32) {
        if let Some(exit) = self.exit.take() {
            let _ = exit.send(code);
        }
    }

    /// Ends the task as lost, at once rather than after the companion's 30 seconds.
    pub(crate) fn report_lost(&mut self) {
        self.report_exit(LOST_SESSION_EXIT_CODE.into());
    }

    /// Whether the connection is over after it was given a code, so nothing is left to wait for.
    pub(crate) fn is_done(&self) -> bool {
        self.exit.is_none() && self.task.is_finished()
    }

    /// Drops the connection without a code, for a task that is gone or was withdrawn.
    pub(crate) fn abort(self) {
        self.task.abort();
    }

    /// Waits until the connection sent its code and closed, or gave up.
    pub(crate) async fn closed(self) {
        let _ = self.task.await;
    }
}

/// What ends a stretch of an owner connection.
enum Event {
    /// The instance exited with this code.
    Exit(i32),
    /// The supervision dropped the connection without a code.
    Withdrawn,
    /// The connection broke, for this reason.
    Dropped(&'static str),
}

/// Why an owner connection couldn't be opened.
#[derive(Debug, Error)]
enum OpenError {
    #[error("failed to connect through the sessions-manager: {0}")]
    Connect(#[from] SessionsManagerClientError),
    #[error("the connection failed: {0}")]
    Connection(#[from] OperatorConnectionError),
    #[error("the connection closed before the companion answered")]
    Closed,
    #[error("the companion didn't answer in time")]
    Timeout,
    /// The companion's mirrord-protocol version doesn't match [`REPLACED_TASK_EXIT_VERSION`].
    #[error("the companion speaks mirrord-protocol {0}")]
    Version(semver::Version),
}

/// Why an exit code wasn't confirmed.
#[derive(Debug, Error)]
enum ReportError {
    #[error("the connection failed: {0}")]
    Connection(#[from] OperatorConnectionError),
    #[error("the companion didn't confirm it in time")]
    Timeout,
}

/// An open owner connection, with the count of its [`ClientMessage::Ping`]s that the companion
/// hasn't answered yet.
struct Link {
    connection: OperatorConnection,
    awaiting_pongs: usize,
}

impl Link {
    fn new(connection: OperatorConnection) -> Self {
        Self {
            connection,
            awaiting_pongs: 0,
        }
    }

    /// Handles a message of the companion that is neither a [`DaemonMessage::Pong`] nor the end
    /// of the connection. Fails when the answer can't be sent.
    async fn handle(&mut self, message: DaemonMessage) -> Result<(), OperatorConnectionError> {
        if let DaemonMessage::OperatorPing(id) = message {
            self.connection
                .send(ClientMessage::OperatorPong(id))
                .await?;
        }
        Ok(())
    }
}

/// Runs the owner connection of one task until it delivered the code of `exit`, or `exit` is
/// dropped.
///
/// Reports on `opened` once the first connection is open. When it can't be opened, no instance
/// starts, and the companion ends the task as lost. Once open, a connection that drops is
/// reopened for as long as the instance runs: the instance's own connection keeps the task alive
/// meanwhile, and the code needs a connection of ours.
async fn own(
    connect_info: SessionsManagerConnectInfo,
    transport: OperatorTransport,
    name: Arc<str>,
    ping: Duration,
    opened: oneshot::Sender<()>,
    mut exit: oneshot::Receiver<i32>,
) {
    let client = match IntproxyClient::with_transport(connect_info, transport) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("{name}: failed to connect to the task: {error}");
            return;
        }
    };
    let Some(link) = open(&client, &name, Instant::now() + OPEN_TIMEOUT).await else {
        return;
    };
    // The receiver is gone only when the task ended meanwhile.
    let _ = opened.send(());

    let mut link = Some(link);
    loop {
        let Some(current) = link.as_mut() else {
            tokio::select! {
                code = &mut exit => {
                    if let Ok(code) = code {
                        deliver(None, code, &client, &name).await;
                    }
                    return;
                }
                reopened = reopen(&client, &name) => match reopened {
                    Some(reopened) => {
                        println!("{name}: reconnected to the task");
                        link = Some(reopened);
                        continue;
                    }
                    None => return,
                },
            }
        };
        match keep_alive(current, &mut exit, ping).await {
            Event::Dropped(reason) => {
                eprintln!("{name}: {reason}, reconnecting");
                link = None;
            }
            Event::Exit(code) => {
                deliver(link.take(), code, &client, &name).await;
                return;
            }
            Event::Withdrawn => return,
        }
    }
}

/// Keeps `link` alive with [`ClientMessage::Ping`], as the intproxy's ping-pong does, and reads
/// and drops whatever else the companion sends, so that its stream doesn't back up, until the
/// instance exits or the connection breaks. A ping still unanswered when the next one is due
/// counts as the connection breaking: through the operator, a connection can stall without
/// closing.
async fn keep_alive(link: &mut Link, exit: &mut oneshot::Receiver<i32>, ping: Duration) -> Event {
    let mut ticks = tokio::time::interval_at(Instant::now() + ping, ping);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            code = &mut *exit => return code.map_or(Event::Withdrawn, Event::Exit),
            _ = ticks.tick() => {
                if link.awaiting_pongs > 0 {
                    return Event::Dropped("the task didn't answer a ping in time");
                }
                if link.connection.send(ClientMessage::Ping).await.is_err() {
                    return Event::Dropped("lost the connection to the task");
                }
                link.awaiting_pongs += 1;
            }
            message = link.connection.next() => match message {
                Some(Ok(DaemonMessage::Pong)) => {
                    link.awaiting_pongs = link.awaiting_pongs.saturating_sub(1);
                }
                Some(Ok(DaemonMessage::Close(_))) | Some(Err(_)) | None => {
                    return Event::Dropped("lost the connection to the task");
                }
                Some(Ok(message)) => {
                    if link.handle(message).await.is_err() {
                        return Event::Dropped("lost the connection to the task");
                    }
                }
            },
        }
    }
}

/// Delivers the instance's exit code to the task, on `link` or on connections opened for it,
/// until the companion confirms it or [`OPEN_TIMEOUT`] passes. Sending a code again is safe: the
/// companion keeps the first one it receives.
async fn deliver(
    mut link: Option<Link>,
    code: i32,
    client: &IntproxyClient<OperatorTransport>,
    name: &str,
) {
    let deadline = Instant::now() + OPEN_TIMEOUT;
    loop {
        let current = match link.take() {
            Some(current) => current,
            None => match open(client, name, deadline).await {
                Some(opened) => opened,
                None => {
                    eprintln!("{name}: failed to report the exit code to the task");
                    return;
                }
            },
        };
        let error = match report(current, code).await {
            Ok(()) => return,
            Err(error) => error,
        };
        if Instant::now() + OPEN_RETRY_DELAY >= deadline {
            eprintln!("{name}: failed to report the exit code to the task: {error}");
            return;
        }
        eprintln!("{name}: failed to report the exit code to the task, retrying: {error}");
        tokio::time::sleep(OPEN_RETRY_DELAY).await;
    }
}

/// Sends the instance's exit code on `link`, and waits for the companion to confirm it, then
/// closes the connection.
///
/// The companion handles a connection's messages in order, so its [`DaemonMessage::Pong`] to
/// the [`ClientMessage::Ping`] sent after the code proves that it took the code. The pongs of
/// the pings that were still unanswered come before it. The companion closing the connection
/// confirms the code too: it does so once it ended the task with it.
async fn report(mut link: Link, code: i32) -> Result<(), ReportError> {
    link.connection
        .send(ClientMessage::ReplacedTaskExit(code))
        .await?;
    link.connection.send(ClientMessage::Ping).await?;
    let mut awaiting_pongs = link.awaiting_pongs + 1;
    tokio::time::timeout(CONFIRM_TIMEOUT, async {
        loop {
            match link.connection.next().await {
                Some(Ok(DaemonMessage::Pong)) => {
                    awaiting_pongs = awaiting_pongs.saturating_sub(1);
                    if awaiting_pongs == 0 {
                        return Ok(());
                    }
                }
                Some(Ok(DaemonMessage::Close(_))) | None => return Ok(()),
                Some(Ok(message)) => link.handle(message).await?,
                Some(Err(error)) => return Err(ReportError::Connection(error)),
            }
        }
    })
    .await
    .map_err(|_| ReportError::Timeout)??;
    let _ = SinkExt::<ClientMessage>::close(&mut link.connection).await;
    Ok(())
}

/// Opens a connection to the task, pinned, trying until `deadline`, and settles its
/// mirrord-protocol version. [`None`] when it can't, and early when the companion is too old to
/// take the exit code.
async fn open(
    client: &IntproxyClient<OperatorTransport>,
    name: &str,
    deadline: Instant,
) -> Option<Link> {
    loop {
        let error = match open_once(client, deadline).await {
            Ok(connection) => return Some(Link::new(connection)),
            Err(OpenError::Version(version)) => {
                too_old(name, &version);
                return None;
            }
            Err(error) => error,
        };
        if Instant::now() + OPEN_RETRY_DELAY >= deadline {
            eprintln!("{name}: failed to connect to the task: {error}");
            return None;
        }
        tokio::time::sleep(OPEN_RETRY_DELAY).await;
    }
}

/// Reopens a dropped connection to the task, pinned, with a backoff capped at
/// [`REOPEN_MAX_DELAY`], for as long as it takes. [`None`] only when the companion is too old to
/// take the exit code. The caller stops it once the instance exits.
async fn reopen(client: &IntproxyClient<OperatorTransport>, name: &str) -> Option<Link> {
    let mut delay = OPEN_RETRY_DELAY;
    let mut reported = false;
    loop {
        match open_once(client, Instant::now() + OPEN_TIMEOUT).await {
            Ok(connection) => return Some(Link::new(connection)),
            Err(OpenError::Version(version)) => {
                too_old(name, &version);
                return None;
            }
            Err(error) if reported.not() => {
                reported = true;
                eprintln!("{name}: failed to reconnect to the task, still trying: {error}");
            }
            Err(_) => {}
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(REOPEN_MAX_DELAY);
    }
}

/// Says that the task's companion is too old to take the exit code of a local process.
fn too_old(name: &str, version: &semver::Version) {
    eprintln!(
        "{name}: the task's mirrord companion is too old to take the exit code of a local \
         process (mirrord-protocol {version}), so no local process runs for it, and it ends as \
         a lost session. Update the task's mirrord remote bootstrap."
    );
}

/// One attempt of [`open`], with the version negotiation the intproxy does, after which the
/// connection announces itself as the task's owner.
async fn open_once(
    client: &IntproxyClient<OperatorTransport>,
    deadline: Instant,
) -> Result<OperatorConnection, OpenError> {
    let (mut connection, _) = client
        .connect(deadline.saturating_duration_since(Instant::now()))
        .await?;
    connection
        .send(ClientMessage::SwitchProtocolVersion(
            mirrord_protocol::VERSION.clone(),
        ))
        .await?;
    let version = tokio::time::timeout_at(deadline, async {
        loop {
            match connection.next().await {
                Some(Ok(DaemonMessage::SwitchProtocolVersionResponse(version))) => {
                    return Ok(version);
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(OpenError::Connection(error)),
                None => return Err(OpenError::Closed),
            }
        }
    })
    .await
    .map_err(|_| OpenError::Timeout)??;

    if REPLACED_TASK_EXIT_VERSION.matches(&version).not() {
        return Err(OpenError::Version(version));
    }
    connection.send(ClientMessage::ReplacedTaskOwner).await?;
    Ok(connection)
}
