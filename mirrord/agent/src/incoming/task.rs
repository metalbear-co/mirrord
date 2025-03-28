use std::{
    collections::{hash_map::Entry, HashMap},
    error::Error,
    fmt,
    ops::Not,
    sync::Arc,
};

use futures::{
    future::{BoxFuture, Shared},
    stream::FuturesUnordered,
    FutureExt, StreamExt,
};
use tokio::sync::{mpsc, oneshot};

use super::{
    connection::RedirectedConnection, error::RedirectorTaskError, steal_handle::StealHandle,
    PortRedirector, Redirected,
};

/// A task responsible for redirecting incoming connections.
///
/// Has to run in the target's network namespace.
/// Only one instance of this task should run in the agent.
pub struct RedirectorTask<R> {
    /// Implements traffic interception.
    redirector: R,
    /// Provides the [`StealHandle`] with this task's failure reason.
    error_tx: oneshot::Sender<RedirectorTaskError>,
    /// Allows for receiving steal requests from the [`StealHandle`].
    message_rx: mpsc::Receiver<StealRequest>,
    /// Allows for detecting dead steal requests.
    dead_channels: FuturesUnordered<DeadChannelFut>,
    /// Active steal requests.
    ///
    /// Maps the port number to the [`StealHandle`]'s exclusive channel.
    steals: HashMap<u16, mpsc::Sender<RedirectedConnection>>,
}

impl<R> RedirectorTask<R>
where
    R: 'static + PortRedirector,
    R::Error: Into<Arc<dyn Error + Send + Sync + 'static>>,
{
    /// Creates a new instance of this task.
    ///
    /// The task has to be run with [`Self::run`] to start redirecting connections.
    pub fn new(redirector: R) -> (Self, StealHandle) {
        let (error_tx, error_rx) = oneshot::channel();
        let (message_tx, message_rx) = mpsc::channel(16);

        let task = Self {
            redirector,
            error_tx,
            message_rx,
            dead_channels: Default::default(),
            steals: Default::default(),
        };

        let task_error = TaskError(error_rx.shared());
        let steal_handle = StealHandle::new(message_tx.clone(), task_error.clone());

        (task, steal_handle)
    }

    async fn run_inner(&mut self) -> Result<(), R::Error> {
        let mut dead_channels = FuturesUnordered::<DeadChannelFut>::new();

        loop {
            tokio::select! {
                next_conn = self.redirector.next_connection() => {
                    let conn = next_conn?;
                    self.handle_connection(conn).await;
                },

                next_message = self.message_rx.recv() => {
                    let Some(message) = next_message else {
                        // All handles dropped, we can exit.
                        break Ok(());
                    };

                    self.handle_message(message).await?;
                },

                Some(dead) = dead_channels.next() => {
                    self.handle_dead_channel(dead).await?;
                }
            }
        }
    }

    /// Handles a redirected connection coming from [`Self::redirector`].
    ///
    /// This function does not do any cleanup if the steal channel is closed,
    /// as the cleanup is handled in [`Self::handle_dead_channel`].
    ///
    /// # Unstolen connections
    ///
    /// If the connection is not stolen, this functions simply drops it.
    /// It happens when our port redirector returns a connection
    /// to a port that is no longer stolen.
    /// We consider this to be an unlikely race condition.
    async fn handle_connection(&mut self, conn: Redirected) {
        let redirected = RedirectedConnection {
            source: conn.source,
            destination: conn.destination,
            stream: conn.stream,
        };

        let Some(steal) = self.steals.get(&redirected.destination.port()) else {
            return;
        };

        let _ = steal.send(redirected).await;
    }

    /// Handles a [`StealRequest`] coming from this task's [`StealHandle`].
    async fn handle_message(&mut self, message: StealRequest) -> Result<(), R::Error> {
        let StealRequest { port, receiver_tx } = message;

        let entry = self.steals.entry(port);

        if matches!(&entry, Entry::Occupied(e) if e.get().is_closed().not()) {
            panic!("detected duplicate steal port subscription for port {port}, StealHandle should not allow this")
        }

        let (conn_tx, conn_rx) = mpsc::channel(32);

        if matches!(entry, Entry::Vacant(..)) {
            self.redirector.add_redirection(port).await?;
        }

        entry.insert_entry(conn_tx.clone());
        self.dead_channels.push(
            async move {
                conn_tx.closed().await;
                port
            }
            .boxed(),
        );

        let _ = receiver_tx.send(conn_rx);

        Ok(())
    }

    /// Called when this task's [`StealHandle`] drops its [`StolenConnectionsRx`].
    async fn handle_dead_channel(&mut self, port: u16) -> Result<(), R::Error> {
        let Entry::Occupied(e) = self.steals.entry(port) else {
            panic!("the stolen connections sender is only removed here");
        };

        if e.get().is_closed().not() {
            // The handle started a new steal and this is a different channel.
            // `DeadChannelFut` for this one was spawned in `handle_message`.
            return Ok(());
        }

        self.redirector.remove_redirection(port).await?;
        if self.steals.is_empty() {
            self.redirector.cleanup().await?;
        }

        Ok(())
    }

    /// Runs this task.
    ///
    /// This should be called only in the target's network namespace.
    pub async fn run(mut self) -> Result<(), RedirectorTaskError> {
        let main_result = self.run_inner().await;
        let cleanup_result = self.redirector.cleanup().await;

        let result = main_result
            .and(cleanup_result)
            .map_err(|error| RedirectorTaskError::RedirectorError(error.into()));

        if let Err(e) = result.clone() {
            let _ = self.error_tx.send(e);
        }

        result
    }
}

/// Channel that represents a port steal made with a [`StealHandle`].
///
/// The handle uses it to receive stolen connections.
pub(super) type StolenConnectionsRx = mpsc::Receiver<RedirectedConnection>;

/// A request to start stealing connections from some port.
///
/// Sent from a [`StealHandle`] to its task.
pub(super) struct StealRequest {
    /// Port to steal.
    pub(super) port: u16,
    /// Will be used to send the [`StolenConnectionsRx`] to the [`StealHandle`],
    /// once the [`RedirectorTask`] completes the port steal.
    pub(super) receiver_tx: oneshot::Sender<StolenConnectionsRx>,
}

/// Type of [`Future`](std::future::Future) used in [`RedirectorTask::dead_channels`].
type DeadChannelFut = BoxFuture<'static, u16>;

/// Can be used to retrieve an error that occurred in the [`RedirectorTask`].
#[derive(Clone)]
pub(super) struct TaskError(Shared<oneshot::Receiver<RedirectorTaskError>>);

impl TaskError {
    /// Resolves when an error occurs in the [`RedirectorTask`].
    pub(super) async fn get(&self) -> RedirectorTaskError {
        self.0
            .clone()
            .await
            .unwrap_or(RedirectorTaskError::Panicked)
    }
}

impl fmt::Debug for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().now_or_never().fmt(f)
    }
}
