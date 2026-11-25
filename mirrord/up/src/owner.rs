//! The owner connection of a replaced task.
//!
//! A task that the watch of a `replace_task` service matched is held before its application
//! starts, until a developer's local process ends it with an exit code. The instance's own session
//! can't send that code: `mirrord exec` replaces itself with the local process and never sees its
//! exit status. So `mirrord up` keeps a mirrord-protocol connection of its own to each such task,
//! the owner connection, from before the instance starts until it exits, and sends the code on it
//! with [`ClientMessage::ReplacedTaskExit`]. The code travels on the same ordered stream as the
//! connection's end, so the two can't race.
//!
//! The workload companion ends a held task with [`LOST_SESSION_EXIT_CODE`] once no client has been
//! connected to it for 30 seconds. The owner connection keeps the task from ending so while the
//! instance's own connection is set up or reconnects, and is opened again when it drops.

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
    time::Instant,
};

/// How long an owner connection is tried each time it's opened: as long as the companion keeps
/// a held task without connections.
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);

/// The pause between two attempts to open an owner connection.
const OPEN_RETRY_DELAY: Duration = Duration::from_secs(1);

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
    /// The connection broke.
    Dropped,
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

/// Runs the owner connection of one task until it sent the code of `exit`, or `exit` is dropped.
///
/// Reports on `opened` once the first connection is open. When it can't be opened, no instance
/// starts, and the companion ends the task as lost.
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
    let Some(connection) = open(&client, &name).await else {
        return;
    };
    // The receiver is gone only when the task ended meanwhile.
    let _ = opened.send(());

    let mut connection = Some(connection);
    loop {
        let event = match connection.as_mut() {
            Some(connection) => keep_alive(connection, &mut exit, ping).await,
            None => match (&mut exit).await {
                Ok(code) => Event::Exit(code),
                Err(_) => Event::Withdrawn,
            },
        };
        match event {
            Event::Dropped => {
                eprintln!("{name}: lost the connection to the task, reconnecting");
                connection = open(&client, &name).await;
            }
            Event::Exit(code) => {
                let connection = match connection {
                    Some(connection) => Some(connection),
                    None => open(&client, &name).await,
                };
                if let Some(connection) = connection {
                    report(connection, code, &name).await;
                }
                return;
            }
            Event::Withdrawn => return,
        }
    }
}

/// Keeps `connection` alive with [`ClientMessage::Ping`], and reads and drops whatever the
/// companion sends, so that its stream doesn't back up, until the instance exits or the
/// connection breaks.
async fn keep_alive(
    connection: &mut OperatorConnection,
    exit: &mut oneshot::Receiver<i32>,
    ping: Duration,
) -> Event {
    let mut ticks = tokio::time::interval_at(Instant::now() + ping, ping);
    loop {
        tokio::select! {
            code = &mut *exit => return code.map_or(Event::Withdrawn, Event::Exit),
            _ = ticks.tick() => {
                if connection.send(ClientMessage::Ping).await.is_err() {
                    return Event::Dropped;
                }
            }
            message = connection.next() => {
                if matches!(message, Some(Ok(_))).not() {
                    return Event::Dropped;
                }
            }
        }
    }
}

/// Sends the instance's exit code to the task, then closes the connection.
async fn report(mut connection: OperatorConnection, code: i32, name: &str) {
    let sent = async {
        connection
            .send(ClientMessage::ReplacedTaskExit(code))
            .await?;
        SinkExt::<ClientMessage>::close(&mut connection).await
    }
    .await;
    if let Err(error) = sent {
        eprintln!("{name}: failed to report the exit code to the task: {error}");
    }
}

/// Opens a connection to the task, pinned, trying for up to [`OPEN_TIMEOUT`], and settles its
/// mirrord-protocol version. [`None`] when it can't, or when the companion is too old to take
/// the exit code.
async fn open(
    client: &IntproxyClient<OperatorTransport>,
    name: &str,
) -> Option<OperatorConnection> {
    let deadline = Instant::now() + OPEN_TIMEOUT;
    loop {
        let error = match open_once(client, deadline).await {
            Ok(connection) => return Some(connection),
            Err(OpenError::Version(version)) => {
                eprintln!(
                    "{name}: the task's mirrord companion is too old to take the exit code of a \
                     local process (mirrord-protocol {version}), so no local process runs for \
                     it, and it ends as a lost session. Update the task's mirrord remote \
                     bootstrap."
                );
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

/// One attempt of [`open`], with the version negotiation the intproxy does.
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

    if REPLACED_TASK_EXIT_VERSION.matches(&version) {
        Ok(connection)
    } else {
        Err(OpenError::Version(version))
    }
}
