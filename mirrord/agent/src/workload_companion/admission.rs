//! The workload companion's side of a task's admission gate.
//!
//! A task opted in to admission holds its application before `main`, in the bootstrap, until it
//! knows whether a developer's local process runs in place of the task's own code. The bootstrap
//! binds a socket at [`ADMISSION_SOCKET_ENV`], and the companion relays [`GateMessage`] lines to
//! it: the sessions-manager's verdict, and for a replaced task, later the exit code the task ends
//! with. The bootstrap decides on its own when nothing comes: the end of the stream before a
//! verdict lets the application start, and after [`GateMessage::Replace`] it ends the task with
//! [`LOST_SESSION_EXIT_CODE`].
//!
//! The companion, not the bootstrap, owns the deadline for a verdict. The bootstrap gives up after
//! [`ADMISSION_TIMEOUT_ENV`] and starts the application, so a `replace` it read late would hold a
//! task that already runs its own code. The companion therefore accepts a verdict only until
//! [`DEADLINE_MARGIN`] before that, then closes the socket and registers again without a gate,
//! which ends any hold the sessions-manager took on the task meanwhile. A `replace` written just
//! before the bootstrap gave up can still sit unread in the socket, so the companion holds the
//! task only once the bootstrap answered it with [`GateAck::Held`], and withdraws it the same way
//! otherwise. Only then does it register again with [`GateState::Held`], which is when the
//! sessions-manager hands the task to the developer's watch, so no local process starts for a
//! task the companion might still withdraw.
//!
//! While the task is held, the companion decides when its developer is gone. The developer's
//! `mirrord up` keeps an owner connection to the task, which it marks with
//! [`ClientMessage::ReplacedTaskOwner`], and reports the local exit code with
//! [`ClientMessage::ReplacedTaskExit`], which the companion takes from any client connection. A
//! task without an owner connection for [`LOST_SESSION_GRACE`] ends with
//! [`LOST_SESSION_EXIT_CODE`]: counted from when the task was held until the first owner
//! connection arrives, and from when the last one closed after that. Only owner connections
//! count. When `mirrord up` is killed without a chance to clean up, the local process and its
//! intproxy can outlive it, and their connections must not keep a task held when nobody is left
//! to report its exit code.
//!
//! [`ClientMessage::ReplacedTaskExit`]: mirrord_protocol::ClientMessage::ReplacedTaskExit
//! [`ClientMessage::ReplacedTaskOwner`]: mirrord_protocol::ClientMessage::ReplacedTaskOwner

use std::{collections::HashSet, ops::Not, path::PathBuf, sync::Arc, time::Duration};

use futures::future::OptionFuture;
use mirrord_remote_layer_protocol::{
    ADMISSION_SOCKET_ENV, ADMISSION_TIMEOUT_ENV, DEFAULT_ADMISSION_TIMEOUT, GateAck, GateMessage,
    LOST_SESSION_EXIT_CODE, gate_exit_code,
};
use mirrord_sessions_manager_client::{AdmissionVerdict, AgentGate, GateState};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::watch,
    time::{Instant, sleep_until, timeout},
};

use crate::util::ClientId;

/// How much earlier than the bootstrap the companion stops accepting a verdict, so that a verdict
/// already written always reaches a bootstrap that still waits for it.
const DEADLINE_MARGIN: Duration = Duration::from_secs(1);

/// How long the companion waits for the bootstrap's [`GateAck::Held`] after writing
/// [`GateMessage::Replace`]. The bootstrap answers as soon as it reads the line, so only a
/// bootstrap that already gave up, or hangs, takes longer.
const HELD_ACK_TIMEOUT: Duration = Duration::from_secs(1);

/// How long a held task waits without an owner connection before it ends with
/// [`LOST_SESSION_EXIT_CODE`]. Long enough for the owner connection to come back after a network
/// blip or an operator restart.
const LOST_SESSION_GRACE: Duration = Duration::from_secs(30);

/// The companion's connection to the admission gate of its task, waiting for a verdict.
pub(crate) struct Admission {
    socket: UnixStream,
    path: PathBuf,
    /// Past it, a verdict no longer reaches the application. [`None`] when the timeout is too
    /// large to be a point in time, and only the bootstrap's own wait bounds the companion's.
    deadline: Option<Instant>,
    clients: AdmissionHandle,
}

impl Admission {
    /// Connects to the gate socket the bootstrap set in [`ADMISSION_SOCKET_ENV`].
    ///
    /// [`None`] when the variable is unset, so the task has no gate, or the connection fails. The
    /// companion then registers without a gate, and the bootstrap starts the application once its
    /// own wait is over.
    pub(crate) async fn connect() -> Option<Self> {
        let path = PathBuf::from(std::env::var_os(ADMISSION_SOCKET_ENV)?);
        let socket = match UnixStream::connect(&path).await {
            Ok(socket) => socket,
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %path.display(),
                    "Failed to connect to the admission gate, so the task runs its own code"
                );
                return None;
            }
        };
        let deadline =
            Instant::now().checked_add(admission_timeout().saturating_sub(DEADLINE_MARGIN));
        tracing::info!(
            path = %path.display(),
            "Holding the application until the task is admitted"
        );

        Some(Self {
            socket,
            path,
            deadline,
            clients: AdmissionHandle::default(),
        })
    }

    /// The handle client connections report to.
    pub(crate) fn handle(&self) -> AdmissionHandle {
        self.clients.clone()
    }

    /// Relays the verdict that `gate` receives before the deadline, and for a replaced task, the
    /// exit code it ends with.
    ///
    /// Returns once the bootstrap has everything it gets from the companion. A verdict that comes
    /// later is dropped by the control plane at `DEBUG`.
    pub(crate) async fn run(mut self, mut gate: AgentGate) {
        let deadline = OptionFuture::from(self.deadline.map(sleep_until));
        let verdict = tokio::select! {
            // A verdict that is ready together with the deadline is already too late.
            biased;
            Some(()) = deadline => {
                tracing::info!(
                    "No admission verdict came in time, so the task runs its own code"
                );
                self.withdraw(&gate);
                return;
            }
            verdict = gate.verdict() => verdict,
        };

        match verdict {
            // The control plane is over, and the companion with it. Closing the socket lets the
            // application start, as without a companion.
            None => {}
            Some(AdmissionVerdict::Continue) => {
                tracing::info!("The task was admitted to run its own code");
                gate.set(None);
                self.write(GateMessage::Continue).await;
            }
            Some(AdmissionVerdict::Replace) => {
                if !self.write(GateMessage::Replace).await || !self.wait_for_held().await {
                    // The bootstrap is gone, gave up waiting or hangs. The application then runs
                    // its own code, or the task ends as lost once the socket closes, and the
                    // developer must not get the task either way.
                    self.withdraw(&gate);
                    return;
                }
                tracing::info!("A developer's local process runs in place of the task's own code");
                gate.set(Some(GateState::Held));
                gate.resubscribe();
                let code = self.clients.hold().await;
                self.write(GateMessage::Exit(code)).await;
            }
        }
    }

    /// Lets the application start, and has the sessions-manager forget the gate at once, so that
    /// a hold it took on the task in the meantime ends.
    fn withdraw(self, gate: &AgentGate) {
        gate.set(None);
        gate.resubscribe();
        // Dropping the socket closes it, which the bootstrap reads as `continue`.
        drop(self.socket);
    }

    /// Whether the bootstrap answered [`GateMessage::Replace`] with [`GateAck::Held`] within
    /// [`HELD_ACK_TIMEOUT`], skipping lines it doesn't know. A failure is logged here.
    async fn wait_for_held(&mut self) -> bool {
        let mut reader = BufReader::new(&mut self.socket);
        let mut line = Vec::new();
        let read = timeout(HELD_ACK_TIMEOUT, async {
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line).await? {
                    0 => break Ok::<_, std::io::Error>(false),
                    _ if GateAck::parse(&String::from_utf8_lossy(&line)) == Some(GateAck::Held) => {
                        break Ok(true);
                    }
                    _ => {}
                }
            }
        })
        .await;

        match read {
            Ok(Ok(true)) => return true,
            Ok(Ok(false)) => tracing::warn!(
                path = %self.path.display(),
                "The admission gate closed before the bootstrap held the task, so the task runs \
                 its own code"
            ),
            Ok(Err(error)) => tracing::warn!(
                %error,
                path = %self.path.display(),
                "Failed to read from the admission gate, so the task runs its own code"
            ),
            Err(_) => tracing::warn!(
                path = %self.path.display(),
                timeout = ?HELD_ACK_TIMEOUT,
                "The bootstrap didn't hold the task in time, so the developer doesn't get it"
            ),
        }
        false
    }

    /// Whether the bootstrap could be sent `message`. A failure is logged here.
    async fn write(&mut self, message: GateMessage) -> bool {
        let result = self.socket.write_all(message.encode().as_bytes()).await;
        if let Err(error) = &result {
            tracing::warn!(
                %error,
                path = %self.path.display(),
                "Failed to write to the admission gate"
            );
        }
        result.is_ok()
    }
}

/// The admission timeout from [`ADMISSION_TIMEOUT_ENV`], read as the bootstrap reads it.
///
/// With 0 or 1 second, the deadline passes as the companion connects, so it withdraws the gate at
/// once and the task can't be replaced.
fn admission_timeout() -> Duration {
    let Some(value) = std::env::var_os(ADMISSION_TIMEOUT_ENV) else {
        return DEFAULT_ADMISSION_TIMEOUT;
    };
    let value = value.to_string_lossy();

    match value.parse() {
        Ok(seconds) => Duration::from_secs(seconds),
        Err(error) => {
            tracing::warn!(
                %error,
                %value,
                default_seconds = DEFAULT_ADMISSION_TIMEOUT.as_secs(),
                "Invalid {ADMISSION_TIMEOUT_ENV}, using the default"
            );
            DEFAULT_ADMISSION_TIMEOUT
        }
    }
}

/// What the client connections of a gated companion tell its [`Admission`].
#[derive(Debug, Default)]
struct Clients {
    /// The open client connections that announced themselves as the task's owner with
    /// [`ClientMessage::ReplacedTaskOwner`](mirrord_protocol::ClientMessage::ReplacedTaskOwner).
    owners: HashSet<ClientId>,
    /// Whether the task is replaced and still waits for the local process's exit code. Cleared
    /// when it ends as lost.
    held: bool,
    /// The first exit code a client reported while the task was held.
    exit_code: Option<i32>,
}

/// Shared by a gated companion's client connections and its [`Admission`], which needs to know
/// which of them own the task, and the exit code they report while the task is held.
#[derive(Clone, Debug, Default)]
pub(crate) struct AdmissionHandle(
    /// client connections --the open owner connections and the first local exit code-->
    /// [`AdmissionHandle::hold`]
    Arc<watch::Sender<Clients>>,
);

impl AdmissionHandle {
    /// Tracks the client connection `client_id` until the returned value is dropped, so that it
    /// stops counting as an owner connection once it closes.
    pub(crate) fn client_connected(&self, client_id: ClientId) -> AdmissionConnection {
        AdmissionConnection {
            clients: self.0.clone(),
            client_id,
        }
    }

    /// Counts the client connection `client_id` as an owner connection of the task until it
    /// closes. Also before the task is held: the owner connection can arrive while the companion
    /// still registers the hold.
    pub(crate) fn replaced_task_owner(&self, client_id: ClientId) {
        self.0
            .send_if_modified(|clients| clients.owners.insert(client_id));
        tracing::debug!(client_id, "A client owns the replaced task");
    }

    /// Ends the held task with `code`, if it's the first code reported while held.
    ///
    /// Before the task is held, after another code, and after the task ended as lost, the code is
    /// ignored: a task the developer doesn't own, or no longer can end, must not end with it.
    pub(crate) fn replaced_task_exit(&self, client_id: ClientId, code: i32) {
        let accepted = self.0.send_if_modified(|clients| {
            let accepted = clients.held && clients.exit_code.is_none();
            if accepted {
                clients.exit_code = Some(code);
            }
            accepted
        });

        if accepted {
            tracing::debug!(client_id, code, "A client reported the local exit code");
        } else {
            tracing::debug!(
                client_id,
                code,
                "Ignoring the exit code of a replaced task, since this task isn't held, already \
                 has one, or ended as lost"
            );
        }
    }

    /// Marks the task held, and waits for the exit code it ends with: the first one a client
    /// reports, or [`LOST_SESSION_EXIT_CODE`] once no owner connection has been open for
    /// [`LOST_SESSION_GRACE`].
    async fn hold(&self) -> u8 {
        self.0.send_modify(|clients| clients.held = true);
        let mut clients = self.0.subscribe();
        let mut unowned_since = None;

        loop {
            let (owned, exit_code) = {
                let clients = clients.borrow_and_update();
                (clients.owners.is_empty().not(), clients.exit_code)
            };

            if let Some(code) = exit_code {
                tracing::info!("Ending the replaced task with the local process's exit code");
                return gate_exit_code(code);
            }

            if owned {
                unowned_since = None;
            } else {
                unowned_since.get_or_insert_with(Instant::now);
            }
            let lost = OptionFuture::from(
                unowned_since.map(|since| sleep_until(since + LOST_SESSION_GRACE)),
            );

            tokio::select! {
                // `self` holds the sender, so this never fails.
                _ = clients.changed() => {}
                Some(()) = lost => {
                    // Atomically with a client reporting a code, so that a code either wins or is
                    // ignored as late.
                    let mut ended = false;
                    self.0.send_if_modified(|clients| {
                        ended = clients.exit_code.is_none();
                        if ended {
                            clients.held = false;
                        }
                        false
                    });
                    if ended {
                        tracing::warn!(
                            grace = ?LOST_SESSION_GRACE,
                            "No owner connection reached the replaced task in time, so it ends as \
                             lost"
                        );
                        return LOST_SESSION_EXIT_CODE;
                    }
                }
            }
        }
    }
}

/// One client connection tracked by [`AdmissionHandle::client_connected`], until dropped.
pub(crate) struct AdmissionConnection {
    /// this client connection --its end--> [`AdmissionHandle::hold`]
    clients: Arc<watch::Sender<Clients>>,
    client_id: ClientId,
}

impl Drop for AdmissionConnection {
    fn drop(&mut self) {
        self.clients
            .send_if_modified(|clients| clients.owners.remove(&self.client_id));
    }
}
