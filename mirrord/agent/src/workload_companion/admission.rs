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
//! which ends any hold the sessions-manager took on the task meanwhile.
//!
//! While the task is held, the companion decides when its developer is gone: the session reports
//! the local exit code with [`ClientMessage::ReplacedTaskExit`] on any client connection, and a
//! task without client connections for [`LOST_SESSION_GRACE`] ends with
//! [`LOST_SESSION_EXIT_CODE`].
//!
//! [`ClientMessage::ReplacedTaskExit`]: mirrord_protocol::ClientMessage::ReplacedTaskExit

use std::{path::PathBuf, sync::Arc, time::Duration};

use futures::future::OptionFuture;
use mirrord_remote_layer_protocol::{
    ADMISSION_SOCKET_ENV, ADMISSION_TIMEOUT_ENV, DEFAULT_ADMISSION_TIMEOUT, GateMessage,
    LOST_SESSION_EXIT_CODE, gate_exit_code,
};
use mirrord_sessions_manager_client::{AdmissionVerdict, AgentGate, GateState};
use tokio::{
    io::AsyncWriteExt,
    net::UnixStream,
    sync::watch,
    time::{Instant, sleep_until},
};

use crate::util::ClientId;

/// How much earlier than the bootstrap the companion stops accepting a verdict, so that a verdict
/// already written always reaches a bootstrap that still waits for it.
const DEADLINE_MARGIN: Duration = Duration::from_secs(1);

/// How long a held task waits without any client connection before it ends with
/// [`LOST_SESSION_EXIT_CODE`]. Long enough for the developer's connections to come back after a
/// network blip or an operator restart.
const LOST_SESSION_GRACE: Duration = Duration::from_secs(30);

/// The companion's connection to the admission gate of its task, waiting for a verdict.
pub(crate) struct Admission {
    socket: UnixStream,
    path: PathBuf,
    /// Past it, a verdict no longer reaches the application.
    deadline: Instant,
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
        let deadline = Instant::now() + admission_timeout().saturating_sub(DEADLINE_MARGIN);
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

    /// The handle client connections report to while the task is held.
    pub(crate) fn handle(&self) -> AdmissionHandle {
        self.clients.clone()
    }

    /// Relays the verdict that `gate` receives before the deadline, and for a replaced task, the
    /// exit code it ends with.
    ///
    /// Returns once the bootstrap has everything it gets from the companion. A verdict that comes
    /// later is dropped by the control plane at `DEBUG`.
    pub(crate) async fn run(mut self, mut gate: AgentGate) {
        let verdict = tokio::select! {
            // A verdict that is ready together with the deadline is already too late.
            biased;
            () = sleep_until(self.deadline) => {
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
                if !self.write(GateMessage::Replace).await {
                    // The bootstrap is gone or gave up waiting, so the application runs its own
                    // code, and the developer must not get the task.
                    self.withdraw(&gate);
                    return;
                }
                tracing::info!("A developer's local process runs in place of the task's own code");
                gate.set(Some(GateState::Held));
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
    /// Client connections being served.
    connections: usize,
    /// Whether the task is replaced and waits for the local process's exit code.
    held: bool,
    /// The first exit code a client reported while the task was held.
    exit_code: Option<i32>,
}

/// Shared by a gated companion's client connections and its [`Admission`], which needs their
/// count and the exit code they report while the task is held.
#[derive(Clone, Debug, Default)]
pub(crate) struct AdmissionHandle(Arc<watch::Sender<Clients>>);

impl AdmissionHandle {
    /// Counts a client connection until the returned value is dropped.
    pub(crate) fn client_connected(&self) -> AdmissionConnection {
        self.0.send_modify(|clients| clients.connections += 1);
        AdmissionConnection(self.0.clone())
    }

    /// Ends the held task with `code`, if it's the first code reported while held.
    ///
    /// Before the task is held, and after another code, the code is ignored: a task the
    /// developer doesn't own, or no longer can end, must not end with it.
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
                "Ignoring the exit code of a replaced task, since this task isn't held or already \
                 has one"
            );
        }
    }

    /// Marks the task held, and waits for the exit code it ends with: the first one a client
    /// reports, or [`LOST_SESSION_EXIT_CODE`] once no client has been connected for
    /// [`LOST_SESSION_GRACE`].
    async fn hold(&self) -> u8 {
        self.0.send_modify(|clients| clients.held = true);
        let mut clients = self.0.subscribe();
        let mut unconnected_since = None;

        loop {
            let (connections, exit_code) = {
                let clients = clients.borrow_and_update();
                (clients.connections, clients.exit_code)
            };

            if let Some(code) = exit_code {
                tracing::info!("Ending the replaced task with the local process's exit code");
                return gate_exit_code(code);
            }

            if connections == 0 {
                unconnected_since.get_or_insert_with(Instant::now);
            } else {
                unconnected_since = None;
            }
            let lost = OptionFuture::from(
                unconnected_since.map(|since| sleep_until(since + LOST_SESSION_GRACE)),
            );

            tokio::select! {
                // `self` holds the sender, so this never fails.
                _ = clients.changed() => {}
                Some(()) = lost => {
                    tracing::warn!(
                        grace = ?LOST_SESSION_GRACE,
                        "No developer connected to the replaced task in time, so it ends as lost"
                    );
                    return LOST_SESSION_EXIT_CODE;
                }
            }
        }
    }
}

/// One client connection counted by [`AdmissionHandle::client_connected`], until dropped.
pub(crate) struct AdmissionConnection(Arc<watch::Sender<Clients>>);

impl Drop for AdmissionConnection {
    fn drop(&mut self) {
        self.0
            .send_modify(|clients| clients.connections = clients.connections.saturating_sub(1));
    }
}
