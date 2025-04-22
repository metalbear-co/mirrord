use std::{
    collections::{hash_map::Entry, HashMap},
    error::Error,
    ops::Not,
    sync::Arc,
};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{
    connection::{
        http::{
            service::{self, ExtractedRequest},
            RedirectedHttp,
        },
        tcp::RedirectedTcp,
        ConnectionInfo, MaybeHttp,
    },
    error::RedirectorTaskError,
    steal_handle::{StealHandle, StealRequest, StolenTraffic},
    PortRedirector, Redirected,
};
use crate::{
    steal::StealTlsHandlerStore,
    util::status::{self, StatusSender},
};

/// A task responsible for redirecting incoming connections.
///
/// # Important
/// 1. Has to run in the target's network namespace.
/// 2. Only one instance of this task should run in the agent.
pub struct RedirectorTask<R> {
    /// Implements traffic interception.
    redirector: R,
    /// For accepting TLS connections and extracting HTTP requests
    /// from redirected connections.
    tls_handlers: StealTlsHandlerStore,
    /// Provides the [`StealHandle`] with this task's failure reason.
    ///
    /// Dropping this sender will be seen as a panic in this task.
    status_tx: StatusSender<RedirectorTaskError>,
    /// Allows for receiving steal requests from the [`StealHandle`].
    external_rx: mpsc::Receiver<StealRequest>,

    internal_tx: mpsc::Sender<InternalMessage>,
    internal_rx: mpsc::Receiver<InternalMessage>,

    ports: HashMap<u16, PortState>,
}

impl<R> RedirectorTask<R>
where
    R: 'static + PortRedirector,
    R::Error: Into<Arc<dyn Error + Send + Sync + 'static>>,
{
    const INTERNAL_CHANNEL_CAPACITY: usize = 16;
    const EXTERNAL_CHANNEL_CAPACITY: usize = 8;

    /// Creates a new instance of this task.
    ///
    /// The task has to be run with [`Self::run`] to start redirecting connections.
    pub fn new(redirector: R, tls_handlers: StealTlsHandlerStore) -> (Self, StealHandle) {
        let (status_tx, status_rx) = status::channel();
        let (external_tx, external_rx) = mpsc::channel(Self::EXTERNAL_CHANNEL_CAPACITY);
        let (internal_tx, internal_rx) = mpsc::channel(Self::INTERNAL_CHANNEL_CAPACITY);

        let task = Self {
            redirector,
            tls_handlers,
            status_tx,
            external_rx,
            internal_tx,
            internal_rx,
            ports: Default::default(),
        };

        let steal_handle = StealHandle::new(external_tx, status_rx);

        (task, steal_handle)
    }

    async fn run_inner(&mut self) -> Result<(), R::Error> {
        loop {
            tokio::select! {
                next_conn = self.redirector.next_connection() => {
                    let conn = next_conn?;
                    self.handle_connection(conn);
                },

                next_message = self.external_rx.recv() => {
                    let Some(message) = next_message else {
                        // All handles dropped, we can exit.
                        self.cleanup().await?;
                        break Ok(());
                    };

                    self.handle_message(message).await?;
                },

                next_message = self.internal_rx.recv() => {
                    let message = next_message.expect("this channel is never closed");
                    match message {
                        InternalMessage::DeadChannel { port, channel } => {
                            self.handle_dead_channel(port, channel).await?;
                        }

                        InternalMessage::ConnInitialized(conn) => {
                            self.handle_connection_initialized(conn).await;
                        }

                        InternalMessage::Request(request, conn_info) => {
                            self.handle_request(request, conn_info).await;
                        }
                    }
                },
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
    fn handle_connection(&mut self, conn: Redirected) {
        if self.ports.contains_key(&conn.destination.port()).not() {
            return;
        }

        let tls_handlers = self.tls_handlers.clone();
        let internal_tx = self.internal_tx.clone();

        tokio::spawn(async move {
            let source = conn.source;
            let destination = conn.destination;

            match MaybeHttp::initialize(conn, &tls_handlers).await {
                Ok(conn) => {
                    let _ = internal_tx
                        .send(InternalMessage::ConnInitialized(conn))
                        .await;
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        %source,
                        %destination,
                        "Failed to determine whether a redirected connection is HTTP",
                    )
                }
            }
        });
    }

    async fn handle_connection_initialized(&mut self, conn: MaybeHttp) {
        let Some(state) = self.ports.get(&conn.info.original_destination.port()) else {
            return;
        };

        let Some(http_version) = conn.http_version else {
            let redirected = RedirectedTcp::new(conn.stream, conn.info);
            let _ = state.steal.send(StolenTraffic::Tcp(redirected)).await;
            return;
        };

        let shutdown = state.http_shutdown.clone().cancelled_owned();

        tokio::spawn(service::extract_requests(
            conn.info,
            self.internal_tx.clone(),
            conn.stream,
            http_version,
            shutdown,
        ));
    }

    async fn handle_request(&mut self, request: ExtractedRequest, conn_info: ConnectionInfo) {
        let Some(state) = self.ports.get(&conn_info.original_destination.port()) else {
            return;
        };

        let redirected = RedirectedHttp::new(conn_info, request);
        let _ = state.steal.send(StolenTraffic::Http(redirected)).await;
    }

    /// Handles a [`StealRequest`] coming from this task's [`StealHandle`].
    async fn handle_message(&mut self, message: StealRequest) -> Result<(), R::Error> {
        let StealRequest { port, receiver_tx } = message;

        let (conn_tx, conn_rx) = mpsc::channel(32);

        match self.ports.entry(port) {
            Entry::Vacant(e) => {
                self.redirector.add_redirection(port).await?;
                e.insert(PortState {
                    steal: conn_tx.clone(),
                    http_shutdown: CancellationToken::new(),
                });
            }
            Entry::Occupied(mut e) => {
                e.get_mut().steal = conn_tx.clone();
            }
        }

        let internal_tx = self.internal_tx.clone();
        tokio::spawn(async move {
            conn_tx.closed().await;
            let _ = internal_tx
                .send(InternalMessage::DeadChannel {
                    port,
                    channel: conn_tx,
                })
                .await;
        });

        let _ = receiver_tx.send(conn_rx);

        Ok(())
    }

    /// Called when this task's [`StealHandle`] drops its [`StolenConnectionsRx`].
    async fn handle_dead_channel(
        &mut self,
        port: u16,
        channel: mpsc::Sender<StolenTraffic>,
    ) -> Result<(), R::Error> {
        let Entry::Occupied(e) = self.ports.entry(port) else {
            panic!("the stolen connections sender is only removed here");
        };

        if e.get().steal.same_channel(&channel).not() {
            // The handle started a new steal and this is a different channel.
            // `DeadChannelFut` for this one was spawned in `handle_message`.
            return Ok(());
        }

        e.remove();

        self.redirector.remove_redirection(port).await?;
        if self.ports.is_empty() {
            self.redirector.cleanup().await?;
        }

        Ok(())
    }

    /// Called the [`StealHandle`] is dropped and this is about to exit.
    ///
    /// Cleans the redirections in [`Self::redirector`].
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
            self.status_tx.send(e);
        }

        result
    }
}

pub(super) enum InternalMessage {
    DeadChannel {
        port: u16,
        channel: mpsc::Sender<StolenTraffic>,
    },
    ConnInitialized(MaybeHttp),
    Request(ExtractedRequest, ConnectionInfo),
}

struct PortState {
    steal: mpsc::Sender<StolenTraffic>,
    http_shutdown: CancellationToken,
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

    use crate::{
        incoming::{test::DummyRedirector, RedirectorTask},
        steal::StealTlsHandlerStore,
        util::path_resolver::InTargetPathResolver,
    };

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn cleanup_on_dead_channel() {
        let (redirector, mut state, _tx) = DummyRedirector::new();
        let (task, mut handle) = RedirectorTask::new(
            redirector,
            StealTlsHandlerStore::new(
                Default::default(),
                InTargetPathResolver::with_root_path("/".into()),
            ),
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
}
