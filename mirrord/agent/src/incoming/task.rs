use std::{
    collections::{hash_map::Entry, HashMap},
    error::{Error, Report},
    fmt,
    ops::Not,
    sync::Arc,
};

use futures::{future::Shared, FutureExt, StreamExt};
use hyper_util::rt::TokioIo;
use tokio::sync::{
    mpsc::{self, error::TrySendError},
    oneshot,
};
use tokio_util::sync::CancellationToken;
use tracing::Level;

use super::{
    connection::{http::RedirectedHttp, tcp::RedirectedTcp, ConnectionInfo, IncomingIO, MaybeHttp},
    error::RedirectorTaskError,
    steal_handle::{StealHandle, StolenTraffic},
    tls::StealTlsHandlerStore,
    PortRedirector, Redirected,
};
use crate::{
    http::extract_requests::{ExtractedRequest, ExtractedRequests},
    incoming::{mirror_handle::MirrorHandle, MirroredTraffic},
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
    /// Allows for receiving steal requests from the [`StealHandle`] and [`MirrorHandle`]s.
    message_rx: mpsc::Receiver<RedirectRequest>,
    /// Maps the port number to its current state.
    ports: HashMap<u16, PortState>,
    /// For communication with helper tasks.
    internal_rx: mpsc::Receiver<InternalMessage>,
    /// For communication with helper tasks.
    internal_tx: mpsc::Sender<InternalMessage>,
    /// For accepting redirected TLS connections.
    tls_store: StealTlsHandlerStore,
}

impl<R> RedirectorTask<R>
where
    R: 'static + PortRedirector,
    R::Error: Into<Arc<dyn Error + Send + Sync + 'static>> + fmt::Display,
{
    /// Creates a new instance of this task.
    ///
    /// The task has to be run with [`Self::run`] to start redirecting connections.
    pub fn new(
        redirector: R,
        tls_store: StealTlsHandlerStore,
    ) -> (Self, StealHandle, MirrorHandle) {
        let (error_tx, error_rx) = oneshot::channel();
        let (message_tx, message_rx) = mpsc::channel(16);
        let (internal_tx, internal_rx) = mpsc::channel(16);

        let task = Self {
            redirector,
            error_tx,
            message_rx,
            ports: Default::default(),
            internal_rx,
            internal_tx,
            tls_store,
        };

        let task_error = TaskError(error_rx.shared());
        let steal_handle = StealHandle::new(message_tx.clone(), task_error.clone());
        let mirror_handle = MirrorHandle::new(message_tx, task_error);

        (task, steal_handle, mirror_handle)
    }

    /// Runs the main [`RedirectorTask`] even loop.
    ///
    /// # Async operations
    ///
    /// Beware **not** to `.await` on any network IO here.
    /// This task is meant to serve multiple clients,
    /// and waiting on IO would prevent it from processing new redirected connections.
    async fn run_inner(&mut self) -> Result<(), R::Error> {
        self.redirector.initialize().await?;

        loop {
            tokio::select! {
                next_conn = self.redirector.next_connection() => {
                    let conn = next_conn?;
                    self.handle_connection(conn);
                },

                next_message = self.message_rx.recv() => {
                    let Some(message) = next_message else {
                        // All handles dropped, we can exit.
                        self.cleanup().await?;
                        break Ok(());
                    };

                    self.handle_client_request(message).await?;
                },

                Some(message) = self.internal_rx.recv() => match message {
                    InternalMessage::DeadChannel(port) => {
                        self.handle_dead_channel(port).await?;
                    }
                    InternalMessage::ConnInitialized(conn) => {
                        self.handle_initialized_connection(conn).await;
                    }
                    InternalMessage::Request(request, info) => {
                        self.handle_extracted_request(request, info).await;
                    }
                }
            }
        }
    }

    /// Handles a redirected connection coming from [`Self::redirector`].
    ///
    /// This function does not do any cleanup if the clients' channels are closed,
    /// as the cleanup is handled in [`Self::handle_dead_channel`].
    ///
    /// # Unsubscribed connections
    ///
    /// If port is no longer stolen/mirrored, this functions simply drops it.
    /// We consider this to be an unlikely race condition.
    #[tracing::instrument(level = Level::TRACE, ret)]
    fn handle_connection(&self, conn: Redirected) {
        let source = conn.source;
        let destination = conn.destination;

        if self.ports.contains_key(&conn.destination.port()).not() {
            tracing::warn!(
                %source,
                %destination,
                "Redirected connection port is no longer subscribed, dropping",
            );
            return;
        };

        let tx = self.internal_tx.clone();
        let tls_store = self.tls_store.clone();
        tokio::spawn(async move {
            match MaybeHttp::detect(conn, &tls_store).await {
                Ok(conn) => {
                    let _ = tx.send(InternalMessage::ConnInitialized(conn)).await;
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        %source,
                        %destination,
                        "HTTP detection failed on a redirected connection",
                    )
                }
            }
        });
    }

    #[tracing::instrument(level = Level::TRACE, ret)]
    async fn handle_initialized_connection(&self, conn: MaybeHttp) {
        let Some(port_state) = self.ports.get(&conn.info.original_destination.port()) else {
            tracing::warn!(
                connection = ?conn,
                "Redirected connection port is no longer subscribed, dropping",
            );
            return;
        };

        let Some(http_version) = conn.http_version else {
            let redirected = RedirectedTcp::new(conn.stream, conn.info);

            for mirror_tx in &port_state.mirror_txs {
                if let Err(TrySendError::Full(..)) =
                    mirror_tx.try_send(MirroredTraffic::Tcp(redirected.mirror()))
                {
                    tracing::warn!(
                        connection = ?redirected,
                        "Mirroring client's traffic channel is full, \
                        client will not receive mirrored traffic",
                    );
                }
            }

            match &port_state.steal_tx {
                Some(steal_tx) => {
                    let _ = steal_tx.send(StolenTraffic::Tcp(redirected)).await;
                }
                None => {
                    redirected.pass_through();
                }
            }

            return;
        };

        let tx = self.internal_tx.clone();
        let token = port_state.http_shutdown.clone();
        let mut requests = ExtractedRequests::new(TokioIo::new(conn.stream), http_version);
        tokio::spawn(async move {
            loop {
                let result = tokio::select! {
                    result = requests.next() => result,
                    _ = token.cancelled() => {
                        requests.graceful_shutdown();
                        continue;
                    },
                };

                let request = match result {
                    None => break,
                    Some(Ok(request)) => request,
                    Some(Err(error)) => {
                        tracing::warn!(
                            error = %Report::new(error),
                            connection = ?conn.info,
                            "Redirected HTTP connection failed",
                        );
                        break;
                    }
                };

                if tx
                    .send(InternalMessage::Request(request, conn.info.clone()))
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        info = ?conn.info,
                        "Redirected HTTP request dropped",
                    );
                    break;
                }
            }
        });
    }

    #[tracing::instrument(level = Level::TRACE, ret)]
    async fn handle_extracted_request(
        &self,
        request: ExtractedRequest<TokioIo<Box<dyn IncomingIO>>>,
        info: ConnectionInfo,
    ) {
        let Some(port_state) = self.ports.get(&info.original_destination.port()) else {
            tracing::warn!(
                ?request,
                ?info,
                "Redirected request port is no longer subscribed, dropping",
            );
            return;
        };

        let redirected = RedirectedHttp::new(info, request);

        for mirror_tx in &port_state.mirror_txs {
            if let Err(TrySendError::Full(..)) =
                mirror_tx.try_send(MirroredTraffic::Http(redirected.mirror()))
            {
                tracing::warn!(
                    request = ?redirected,
                    "Mirroring client's traffic channel is full, \
                    client will not receive mirrored request",
                );
            }
        }

        match &port_state.steal_tx {
            Some(steal_tx) => {
                let _ = steal_tx.send(StolenTraffic::Http(redirected)).await;
            }
            None => redirected.pass_through(),
        }
    }

    /// Handles a [`RedirectRequest`] coming from this task's handles.
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    async fn handle_client_request(&mut self, message: RedirectRequest) -> Result<(), R::Error> {
        match message {
            RedirectRequest::Mirror { port, receiver_tx } => {
                let (conn_tx, conn_rx) = mpsc::channel(32);

                match self.ports.entry(port) {
                    Entry::Vacant(e) => {
                        self.redirector.add_redirection(port).await?;
                        e.insert_entry(PortState {
                            steal_tx: None,
                            mirror_txs: vec![conn_tx.clone()],
                            http_shutdown: Default::default(),
                        });
                    }
                    Entry::Occupied(mut e) => {
                        e.get_mut().mirror_txs.push(conn_tx.clone());
                    }
                };

                let tx = self.internal_tx.clone();
                tokio::spawn(async move {
                    conn_tx.closed().await;
                    let _ = tx.send(InternalMessage::DeadChannel(port)).await;
                });

                let _ = receiver_tx.send(conn_rx);
            }

            RedirectRequest::Steal { port, receiver_tx } => {
                let (conn_tx, conn_rx) = mpsc::channel(32);

                match self.ports.entry(port) {
                    Entry::Vacant(e) => {
                        self.redirector.add_redirection(port).await?;
                        e.insert_entry(PortState {
                            steal_tx: Some(conn_tx.clone()),
                            mirror_txs: Default::default(),
                            http_shutdown: Default::default(),
                        });
                    }
                    Entry::Occupied(mut e) => {
                        e.get_mut().steal_tx.replace(conn_tx.clone());
                    }
                }

                let tx = self.internal_tx.clone();
                tokio::spawn(async move {
                    conn_tx.closed().await;
                    let _ = tx.send(InternalMessage::DeadChannel(port)).await;
                });

                let _ = receiver_tx.send(conn_rx);
            }
        }

        Ok(())
    }

    /// Called when one of the ports require inspection - the redirection might be stale.
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    async fn handle_dead_channel(&mut self, port: u16) -> Result<(), R::Error> {
        let Entry::Occupied(mut e) = self.ports.entry(port) else {
            return Ok(());
        };

        let PortState {
            steal_tx,
            mirror_txs,
            ..
        } = e.get_mut();

        *steal_tx = steal_tx.take().filter(|tx| tx.is_closed().not());
        mirror_txs.retain(|tx| tx.is_closed().not());

        if steal_tx.is_none() && mirror_txs.is_empty() {
            e.remove();
            self.redirector.remove_redirection(port).await?;
            if self.ports.is_empty() {
                self.redirector.cleanup().await?;
            }
            return Ok(());
        }

        Ok(())
    }

    /// Called when all handles are dropped and this task is about to exit.
    ///
    /// Cleans the redirections in [`Self::redirector`].
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    async fn cleanup(&mut self) -> Result<(), R::Error> {
        for port in std::mem::take(&mut self.ports).into_keys() {
            self.redirector.remove_redirection(port).await?;
        }

        self.redirector.cleanup().await
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

impl<R> fmt::Debug for RedirectorTask<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RedirectorTask")
            .field("ports", &self.ports)
            .finish()
    }
}

/// Channel that represents a port steal made with a [`StealHandle`].
///
/// The handle uses it to receive stolen connections.
pub type StolenConnectionsRx = mpsc::Receiver<StolenTraffic>;

/// Channel that represents a port mirror made with a [`MirrorHandle`].
///
/// The handle uses it to receive mirrored connections.
pub type MirroredConnectionsRx = mpsc::Receiver<MirroredTraffic>;

/// A request to start redirecting connections from some port.
///
/// Sent from a [`StealHandle`] or a [`MirrorHandle`] to its task.
pub enum RedirectRequest {
    Steal {
        port: u16,
        receiver_tx: oneshot::Sender<StolenConnectionsRx>,
    },
    Mirror {
        port: u16,
        receiver_tx: oneshot::Sender<MirroredConnectionsRx>,
    },
}

impl fmt::Debug for RedirectRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mirror { port, .. } => f
                .debug_struct("Mirror")
                .field("port", port)
                .finish_non_exhaustive(),
            Self::Steal { port, .. } => f
                .debug_struct("Steal")
                .field("port", port)
                .finish_non_exhaustive(),
        }
    }
}

/// Can be used to retrieve an error that occurred in the [`RedirectorTask`].
#[derive(Clone)]
pub struct TaskError(Shared<oneshot::Receiver<RedirectorTaskError>>);

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

/// Messages sent by [`RedirectorTask`]'s helper tasks.
enum InternalMessage {
    /// One of the clients' channels was closed for a port.
    DeadChannel(u16),
    ConnInitialized(MaybeHttp),
    Request(
        ExtractedRequest<TokioIo<Box<dyn IncomingIO>>>,
        ConnectionInfo,
    ),
}

/// State of a single port in the [`RedirectorTask`].
struct PortState {
    /// Stealer's traffic channel.
    steal_tx: Option<mpsc::Sender<StolenTraffic>>,
    /// Mirrorers' traffic channel.
    mirror_txs: Vec<mpsc::Sender<MirroredTraffic>>,
    /// Used to initiate a graceful shutdown of stolen HTTP connections,
    /// once the all clients cancel their subscriptions.
    http_shutdown: CancellationToken,
}

impl fmt::Debug for PortState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PortState")
            .field(
                "has_stealer",
                &self
                    .steal_tx
                    .as_ref()
                    .is_some_and(|tx| tx.is_closed().not()),
            )
            .field("mirrorers", &self.mirror_txs.len())
            .finish()
    }
}

impl Drop for PortState {
    fn drop(&mut self) {
        self.http_shutdown.cancel();
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use rstest::rstest;

    use crate::incoming::{test::DummyRedirector, RedirectorTask};

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn cleanup_on_dead_channel() {
        let (redirector, mut state, _tx) = DummyRedirector::new();
        let (task, mut handle, _) = RedirectorTask::new(redirector, Default::default());
        tokio::spawn(task.run());

        handle.steal(80).await.unwrap();
        assert!(state.borrow().has_redirections([80]));

        handle.stop_steal(80);
        state
            .wait_for(|state| state.has_redirections([]))
            .await
            .unwrap();

        handle.steal(81).await.unwrap();
        assert!(state.borrow().has_redirections([81]));

        std::mem::drop(handle);
        state
            .wait_for(|state| state.has_redirections([]))
            .await
            .unwrap();
    }
}
