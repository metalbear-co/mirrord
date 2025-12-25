use std::{
    collections::{HashMap, hash_map::Entry},
    error::{Error, Report},
    fmt,
    ops::Not,
    sync::Arc,
};

use futures::{FutureExt, StreamExt, future::Shared};
use hyper_util::rt::TokioIo;
use mirrord_agent_env::envs;
use tokio::{
    sync::{
        mpsc::{self, error::TrySendError},
        oneshot,
    },
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use tracing::Level;

use super::{
    PortRedirector, Redirected,
    connection::{ConnectionInfo, MaybeHttp, http::RedirectedHttp, tcp::RedirectedTcp},
    error::RedirectorTaskError,
    steal_handle::{StealHandle, StolenTraffic},
    tls::StealTlsHandlerStore,
};
use crate::{
    http::extract_requests::{ExtractedRequest, ExtractedRequests},
    incoming::{MirroredTraffic, mirror_handle::MirrorHandle},
};

/// A task responsible for redirecting incoming connections.
///
/// Has to run in the target's network namespace.
/// Only one instance of this task should run in the agent.
///
/// # Implementation
///
/// This task is meant to be use via [`StealHandle`] and [`MirrorHandle`]s
/// returned from [`Self::new`].
///
/// The handles use a common [`mpsc::channel`] to send port subscription requests.
/// Each port subscription requests is represented by a separate [`mpsc::channel`],
/// through which the task sends redirected traffic.
///
/// End of port subscription is signaled by dropping the [`mpsc::Receiver`]
/// from the [`StealHandle`]/[`MirrorHandle`].
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
    /// Configuration
    config: RedirectorTaskConfig,
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
        config: RedirectorTaskConfig,
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
            config,
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
                    InternalMessage::DeadChannel(port)
                     => {
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
    /// If port is no longer stolen/mirrored but there are other
    /// active connections on the same port, the connection is
    /// unconditionally passed through. Otherwise, the connection is
    /// dropped. We consider this to be an unlikely race condition.
    #[tracing::instrument(level = Level::TRACE, ret)]
    fn handle_connection(&mut self, conn: Redirected) {
        let source = conn.source;
        let destination = conn.destination;

        let Entry::Occupied(mut e) = self.ports.entry(destination.port()) else {
            tracing::warn!(
                %source,
                %destination,
                "Redirected connection port is no longer subscribed and has no active connections, dropping",
            );
            return;
        };

        let state = e.get_mut();
        if state.mirror_txs.is_empty().not() || state.steal_tx.is_some() {
            let tx = self.internal_tx.clone();
            let tls_store = self.tls_store.clone();
            let shutdown = state.shutdown.child_token();
            Self::spawn_tracked_connection(
                self.internal_tx.clone(),
                destination.port(),
                state,
                async move {
                    let detection_result = tokio::select! {
                        r = MaybeHttp::detect(conn, &tls_store) => r,
                        _ = shutdown.cancelled() => {
                            tracing::debug!("Shutting down redirected connection during HTTP detection");
                            return;
                        }
                    };

                    match detection_result {
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
                },
            );
        } else {
            let local_addr = match conn.stream.local_addr() {
                Ok(addr) => addr,
                Err(err) => {
                    tracing::error!(
                        ?err,
                        "failed to acquire local address for connection arriving on inactive port."
                    );
                    return;
                }
            };

            let info = ConnectionInfo {
                original_destination: destination,
                local_addr,
                peer_addr: source,
                tls_connector: None,
            };

            let shutdown = state.shutdown.child_token();
            Self::spawn_tracked_connection(
                self.internal_tx.clone(),
                destination.port(),
                e.get_mut(),
                async move {
                    tracing::debug!("connection arrived on inactive port, passing through");
                    if let Err(err) = RedirectedTcp::new(Box::new(conn.stream), info)
                        .pass_through(shutdown)
                        .await
                    {
                        tracing::error!(
                            ?err,
                            "error joining inactive port redirected connection IO task"
                        );
                    }
                },
            );
        }
    }

    #[tracing::instrument(level = Level::TRACE, ret)]
    async fn handle_initialized_connection(&mut self, conn: MaybeHttp) {
        let port = conn.info.original_destination.port();
        let Some(port_state) = self.ports.get_mut(&port) else {
            tracing::warn!(
                connection = ?conn,
                "Redirected connection port is no longer subscribed, dropping",
            );
            return;
        };

        let Some(http_version) = conn.http_version else {
            let mut redirected = RedirectedTcp::new(conn.stream, conn.info);

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

            let join_handle = match &port_state.steal_tx {
                Some(steal_tx) => {
                    let (tx, rx) = oneshot::channel();
                    let _ = steal_tx
                        .send(StolenTraffic::Tcp {
                            conn: redirected,
                            join_handle_tx: tx,
                            shutdown: port_state.shutdown.child_token(),
                        })
                        .await;

                    rx.await.expect("TcpStealerTask dropped oneshot tx for returning JoinHandle to IO task for TCP connection")
                }
                None => redirected.pass_through(port_state.shutdown.child_token()),
            };

            Self::spawn_tracked_connection(self.internal_tx.clone(), port, port_state, async {
                if let Err(err) = join_handle.await {
                    tracing::warn!(?err, "Redirected passthrough task returned JoinError");
                }
            });

            return;
        };

        let tx = self.internal_tx.clone();
        let token = port_state.shutdown.clone();
        let mut requests = ExtractedRequests::new(TokioIo::new(conn.stream), http_version);

        Self::spawn_tracked_connection(self.internal_tx.clone(), port, port_state, async move {
            let mut shutting_down = false;
            loop {
                let result = tokio::select! {
                    result = requests.next() => result,
                    _ = token.cancelled(), if shutting_down.not() => {
                        tracing::debug!(
                            connection = ?conn.info,
                            "Gracefully shutting down a redirected HTTP connection",
                        );
                        // After starting the graceful shutdown,
                        // `requests` iterator will eventually finish on its own.
                        requests.graceful_shutdown();
                        shutting_down = true;
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
                    .send(InternalMessage::Request(request, conn.info.clone().into()))
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
    async fn handle_extracted_request(&self, request: ExtractedRequest, info: Arc<ConnectionInfo>) {
        let Some(port_state) = self.ports.get(&info.original_destination.port()) else {
            tracing::warn!(
                ?request,
                ?info,
                "Redirected request port is no longer subscribed, dropping",
            );
            return;
        };

        let mut redirected = RedirectedHttp::new(info, request, self.config.clone());

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

    /// Handles a [`RedirectRequest`] coming from one of this task's handles.
    ///
    /// Spawns a helper task that waits for the subscription channel to close,
    /// and sends [`InternalMessage::DeadChannel`] back to the [`RedirectorTask`].
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    async fn handle_client_request(&mut self, message: RedirectRequest) -> Result<(), R::Error> {
        match message {
            RedirectRequest::Mirror { port, receiver_tx } => {
                let (conn_tx, conn_rx) = mpsc::channel(32);

                match self.ports.entry(port) {
                    Entry::Vacant(e) => {
                        tracing::debug!(
                            from_port = port,
                            "Creating a new port redirection for a mirroring client"
                        );
                        self.redirector.add_redirection(port).await?;
                        e.insert_entry(PortState {
                            steal_tx: None,
                            mirror_txs: vec![conn_tx.clone()],
                            shutdown: Default::default(),
                            connections: Default::default(),
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
                        tracing::debug!(
                            from_port = port,
                            "Creating a new port redirection for a stealing client"
                        );
                        self.redirector.add_redirection(port).await?;
                        e.insert_entry(PortState {
                            steal_tx: Some(conn_tx.clone()),
                            mirror_txs: Default::default(),
                            shutdown: Default::default(),
                            connections: Default::default(),
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

    /// Called when [`InternalMessage::DeadChannel`] is received from a helper task.
    ///
    /// One of the subscription channels may be closed. We need to
    /// check the related [`PortState`].
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    async fn handle_dead_channel(&mut self, port: u16) -> Result<(), R::Error> {
        let Entry::Occupied(mut e) = self.ports.entry(port) else {
            return Ok(());
        };

        let state = e.get_mut();

        let PortState {
            steal_tx,
            mirror_txs,
            connections,
            ..
        } = state;

        *steal_tx = steal_tx.take().filter(|tx| tx.is_closed().not());
        mirror_txs.retain(|tx| tx.is_closed().not());

        // Drain finished connections
        while let Some(joined) = connections.try_join_next() {
            if let Err(err) = joined {
                tracing::warn!(?err, "IO task returned JoinError");
            }
        }

        // Remove if the [`PortState`] is no longer needed.
        if mirror_txs.is_empty() && connections.is_empty() && steal_tx.is_none() {
            e.remove().graceful_shutdown().await;
            self.redirector.remove_redirection(port).await?;
            if self.ports.is_empty() {
                self.redirector.cleanup().await?;
            }
            return Ok(());
        }

        Ok(())
    }

    fn spawn_tracked_connection<F>(
        tx: mpsc::Sender<InternalMessage>,
        port: u16,
        port_state: &mut PortState,
        f: F,
    ) where
        F: Future<Output = ()> + Send + 'static,
    {
        // Drain any finished connections to prevent OOM
        while let Some(joined) = port_state.connections.try_join_next() {
            if let Err(err) = joined {
                tracing::warn!(?err, "IO task returned JoinError");
            }
        }

        port_state.connections.spawn(async move {
            f.await;

            tx.send(InternalMessage::DeadChannel(port))
                .await
                .expect("RedirectorTask exit before child task");
        });
    }

    /// Called when all handles are dropped and this task is about to exit.
    ///
    /// Cleans the redirections in [`Self::redirector`].
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    async fn cleanup(&mut self) -> Result<(), R::Error> {
        for (port, state) in std::mem::take(&mut self.ports) {
            state.graceful_shutdown().await;
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

#[derive(Debug, Clone)]
pub struct RedirectorTaskConfig {
    /// Inject `Mirrord-Agent` headers into responses to stolen requests
    pub inject_headers: bool,
}

impl RedirectorTaskConfig {
    pub fn from_env() -> Self {
        Self {
            inject_headers: envs::INJECT_HEADERS.from_env_or_default(),
        }
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
    /// One of the clients' channels was closed for a certain port.
    ///
    /// This means an end of port subscription.
    /// The related [`PortState`] should be inspected, adjusted, and possibly removed (if all
    /// subscriptions are gone).
    ///
    /// Each port subscription results in spawning a separate helper task,
    /// that waits for the subscription channel to close, and sends this message to the
    /// [`RedirectorTask`].
    DeadChannel(u16),
    /// HTTP detection finished on a redirected connection.
    ConnInitialized(MaybeHttp),
    /// An HTTP request was extracted from a redirected connection.
    Request(ExtractedRequest, Arc<ConnectionInfo>),
}

/// State of a single port in the [`RedirectorTask`].
struct PortState {
    /// Stealer's traffic channel.
    steal_tx: Option<mpsc::Sender<StolenTraffic>>,
    /// Mirrorers' traffic channel.
    mirror_txs: Vec<mpsc::Sender<MirroredTraffic>>,
    /// Used to initiate a graceful shutdown of redirected
    /// connections, once the all clients cancel their subscriptions.
    shutdown: CancellationToken,
    /// Used to track connection IO tasks and wait for their graceful shutdown.
    connections: JoinSet<()>,
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

impl PortState {
    /// Tell and wait for all connections to gracefully shut down.
    /// This function is essentially `AsyncDrop`, and it should always
    /// be called before removing the redirection.
    async fn graceful_shutdown(mut self) {
        self.shutdown.cancel();
        while let Some(joined) = self.connections.join_next().await {
            if let Err(err) = joined {
                tracing::warn!(?err, "IO task returned JoinError");
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::{ops::Not, time::Duration};

    use bytes::Bytes;
    use http_body_util::Empty;
    use hyper_util::rt::TokioIo;
    use rstest::rstest;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use crate::incoming::{
        RedirectorTask, RedirectorTaskConfig, StolenTraffic, test::DummyRedirector,
    };

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn passthrough_connections_on_inactive_ports(
        #[values(true, false)] first_one_first: bool,
    ) {
        let (redirector, mut state, mut tx) = DummyRedirector::new();
        let (task, mut handle, _) = RedirectorTask::new(
            redirector,
            Default::default(),
            RedirectorTaskConfig::from_env(),
        );
        tokio::spawn(task.run());

        // Make sure connections are now passed through
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        handle.steal(port).await.unwrap();
        assert!(state.borrow().has_redirections([port]));

        let mut tcp = tx.make_connection(listener.local_addr().unwrap()).await;
        tcp.write_all(b"def not http\r\n\r\n").await.unwrap();

        let StolenTraffic::Tcp {
            conn: rtcp,
            join_handle_tx,
            shutdown,
        } = handle.next().await.unwrap().unwrap()
        else {
            panic!("falsely detected HTTP traffic");
        };

        join_handle_tx
            .send(tokio::spawn(async move {
                let mut io = rtcp.into_io();
                let mut buf = [0; 16];
                io.read_exact(&mut buf).await.unwrap();
                assert_eq!(&buf, b"def not http\r\n\r\n");

                io.read_exact(&mut buf[..4]).await.unwrap();
                assert_eq!(&buf[..4], b"ping");

                io.write_all(b"pong").await.unwrap();

                tokio::select! {
                    _ = shutdown.cancelled() => {},
                    r = io.read(&mut buf) => {
                        // We never rx any data, just use this to
                        // detect termination.
                        assert!(r.is_ok_and(|c| c == 0))
                    }
                }
            }))
            .unwrap();

        handle.stop_steal(port);
        // Wait a little, ensure redirection hasn't been removed.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(state.borrow().has_redirections([port]));

        // Make sure connection is stil alive.
        tcp.write_all(b"ping").await.unwrap();
        let mut buf = [0; 4];
        tcp.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");

        // Make sure connections are now being passed through.
        let tcp2 = tx.make_connection(listener.local_addr().unwrap()).await;
        let (rtcp2, _) = listener.accept().await.unwrap();

        if first_one_first {
            drop(tcp);

            // Wait a little, ensure redirection still hasn't been removed.
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(state.borrow().has_redirections([port]));

            drop((tcp2, rtcp2));
        } else {
            drop((tcp2, rtcp2));

            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(state.borrow().has_redirections([port]));

            drop(tcp);
        }

        // NOW the redirection should be gone (because there are no more active connections).
        state
            .wait_for(|state| state.has_redirections([]))
            .await
            .unwrap();
    }

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn cleanup_on_dead_channel() {
        let (redirector, mut state, _tx) = DummyRedirector::new();
        let (task, mut handle, _) = RedirectorTask::new(
            redirector,
            Default::default(),
            RedirectorTaskConfig::from_env(),
        );
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

    /// Regression test for a bug with HTTP graceful shutdown.
    ///
    /// Verifies that [`RedirectorTask`] can handle port unsubscribe during an HTTP exchange.
    ///
    /// See <https://github.com/metalbear-co/mirrord/commit/e7805085d8fb61f94b04ac01254b61be86fad3a0>.
    #[rstest]
    #[timeout(Duration::from_secs(5))]
    // We explicitly use the `current_thread` flavor,
    // as the bug was about hugging the Tokio runtime thread.
    #[tokio::test(flavor = "current_thread")]
    async fn http_graceful_shutdown_regression() {
        let (redirector, mut state, mut conn_tx) = DummyRedirector::new();
        let (task, mut handle, _) = RedirectorTask::new(
            redirector,
            Default::default(),
            RedirectorTaskConfig::from_env(),
        );
        let redirector_task = tokio::spawn(task.run());

        handle.steal(80).await.unwrap();
        let client_conn = conn_tx
            .make_connection("127.0.0.1:80".parse().unwrap())
            .await;

        let http_client_task = tokio::spawn(async {
            let (mut sender, client_conn) =
                hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(TokioIo::new(client_conn))
                    .await
                    .unwrap();
            tokio::spawn(client_conn);
            sender.ready().await.unwrap();
            let response = sender
                .send_request(hyper::Request::new(Default::default()))
                .await
                .unwrap();
            response.status()
        });

        let StolenTraffic::Http(http) = handle.next().await.unwrap().unwrap() else {
            unreachable!()
        };

        // Stealing client unsubscribes the port.
        // This should trigger graceful shutdown on the HTTP connection.
        handle.stop_steal(80);

        // Stealing client exits.
        // The HTTP client should get 502.
        std::mem::drop(http);
        let response_code = http_client_task.await.unwrap();
        assert_eq!(response_code, hyper::StatusCode::BAD_GATEWAY);

        // Wait for the RedirectorTask to process.
        state.wait_for(|state| state.dirty.not()).await.unwrap();

        // The whole agent exits.
        // Redirector task should exit.
        std::mem::drop(handle);
        redirector_task.await.unwrap().unwrap();
    }
}
