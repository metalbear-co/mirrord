use std::{
    collections::{hash_map::Entry, HashMap},
    error::Error,
    ops::Not,
    sync::Arc,
};

use tokio::sync::{mpsc, oneshot};
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
    mirror_handle::{MirrorHandle, MirroredTraffic},
    steal_handle::{StealHandle, StolenTraffic},
    PortRedirector, Redirected,
};
use crate::{
    steal::tls::StealTlsHandlerStore,
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
    /// Provides the handles with this task's failure reason.
    ///
    /// Dropping this sender will be seen as a panic in this task.
    status_tx: StatusSender<RedirectorTaskError>,
    /// Allows for receiving steal requests from the handles.
    external_rx: mpsc::Receiver<RedirectionRequest>,

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
    pub fn new(
        redirector: R,
        tls_handlers: StealTlsHandlerStore,
    ) -> (Self, StealHandle, MirrorHandle) {
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

        let steal_handle = StealHandle::new(external_tx.clone(), status_rx.clone());
        let mirror_handle = MirrorHandle::new(external_tx.clone(), status_rx.clone());

        (task, steal_handle, mirror_handle)
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
                        InternalMessage::DeadSteal { port, channel } => {
                            self.handle_dead_steal(port, channel).await?;
                        }

                        InternalMessage::DeadMirror { port, channel } => {
                            self.handle_dead_mirror(port, channel).await?;
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
            let mut redirected = RedirectedTcp::new(conn.stream, conn.info);

            for mirror in &state.mirrors {
                let Ok(permit) = mirror.try_reserve() else {
                    continue;
                };

                permit.send(MirroredTraffic::Tcp(redirected.mirror()));
            }

            if let Some(steal) = &state.steal {
                let _ = steal.send(StolenTraffic::Tcp(redirected)).await;
            }

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

        let mut redirected = RedirectedHttp::new(conn_info, request);

        for mirror in &state.mirrors {
            let Ok(permit) = mirror.try_reserve() else {
                continue;
            };

            permit.send(MirroredTraffic::Http(redirected.mirror()));
        }

        if let Some(steal) = &state.steal {
            let _ = steal.send(StolenTraffic::Http(redirected)).await;
        }
    }

    /// Handles a [`StealRequest`] coming from this task's [`StealHandle`].
    async fn handle_message(&mut self, message: RedirectionRequest) -> Result<(), R::Error> {
        match message {
            RedirectionRequest::Mirror { port, receiver_tx } => {
                let (conn_tx, conn_rx) = mpsc::channel(32);

                match self.ports.entry(port) {
                    Entry::Vacant(e) => {
                        self.redirector.add_redirection(port).await?;
                        e.insert(PortState {
                            steal: None,
                            mirrors: vec![conn_tx.clone()],
                            http_shutdown: CancellationToken::new(),
                        });
                    }

                    Entry::Occupied(mut e) => {
                        e.get_mut().mirrors.push(conn_tx.clone());
                    }
                }

                let internal_tx = self.internal_tx.clone();
                tokio::spawn(async move {
                    conn_tx.closed().await;
                    let _ = internal_tx
                        .send(InternalMessage::DeadMirror {
                            port,
                            channel: conn_tx,
                        })
                        .await;
                });

                let _ = receiver_tx.send(conn_rx);
            }

            RedirectionRequest::Steal { port, receiver_tx } => {
                let (conn_tx, conn_rx) = mpsc::channel(32);

                match self.ports.entry(port) {
                    Entry::Vacant(e) => {
                        self.redirector.add_redirection(port).await?;
                        e.insert(PortState {
                            steal: Some(conn_tx.clone()),
                            mirrors: Default::default(),
                            http_shutdown: CancellationToken::new(),
                        });
                    }

                    Entry::Occupied(mut e) => {
                        e.get_mut().steal.replace(conn_tx.clone());
                    }
                }

                let internal_tx = self.internal_tx.clone();
                tokio::spawn(async move {
                    conn_tx.closed().await;
                    let _ = internal_tx
                        .send(InternalMessage::DeadSteal {
                            port,
                            channel: conn_tx,
                        })
                        .await;
                });

                let _ = receiver_tx.send(conn_rx);
            }
        }

        Ok(())
    }

    /// Called when this task's [`StealHandle`] drops its traffic [`mpsc::Receiver`].
    async fn handle_dead_steal(
        &mut self,
        port: u16,
        channel: mpsc::Sender<StolenTraffic>,
    ) -> Result<(), R::Error> {
        let Entry::Occupied(e) = self.ports.entry(port) else {
            return Ok(());
        };

        let same_channel = e
            .get()
            .steal
            .as_ref()
            .is_some_and(|c| c.same_channel(&channel));
        if same_channel.not() {
            return Ok(());
        }

        if e.get().mirrors.is_empty().not() {
            return Ok(());
        }

        e.remove();

        self.redirector.remove_redirection(port).await?;
        if self.ports.is_empty() {
            self.redirector.cleanup().await?;
        }

        Ok(())
    }

    /// Called when this task's [`MirrorHandle`] drops its traffic [`mpsc::Receiver`].
    async fn handle_dead_mirror(
        &mut self,
        port: u16,
        channel: mpsc::Sender<MirroredTraffic>,
    ) -> Result<(), R::Error> {
        let Entry::Occupied(mut e) = self.ports.entry(port) else {
            return Ok(());
        };

        let position = e
            .get()
            .mirrors
            .iter()
            .position(|c| c.same_channel(&channel));
        let Some(position) = position else {
            return Ok(());
        };
        e.get_mut().mirrors.remove(position);

        if e.get().mirrors.is_empty().not() || e.get().steal.is_some() {
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
    DeadSteal {
        port: u16,
        channel: mpsc::Sender<StolenTraffic>,
    },
    DeadMirror {
        port: u16,
        channel: mpsc::Sender<MirroredTraffic>,
    },
    ConnInitialized(MaybeHttp),
    Request(ExtractedRequest, ConnectionInfo),
}

struct PortState {
    steal: Option<mpsc::Sender<StolenTraffic>>,
    mirrors: Vec<mpsc::Sender<MirroredTraffic>>,
    http_shutdown: CancellationToken,
}

impl Drop for PortState {
    fn drop(&mut self) {
        self.http_shutdown.cancel();
    }
}

/// A request for the [`RedirectorTask`] to start redirecting connections
/// from some port.
///
/// Sent from [`StealHandle`]/[`MirrorHandle`] to its task.
pub(super) enum RedirectionRequest {
    Steal {
        /// Port to steal.
        port: u16,
        /// Will be used to send the [`StolenConnectionsRx`] to the [`StealHandle`],
        /// once the [`RedirectorTask`] completes the port steal.
        receiver_tx: oneshot::Sender<mpsc::Receiver<StolenTraffic>>,
    },

    Mirror {
        /// Port to mirror.
        port: u16,
        /// Will be used to send the traffic receiver to the [`MirrorHandle`],
        /// once the [`RedirectorTask`] completes the port mirror.
        receiver_tx: oneshot::Sender<mpsc::Receiver<MirroredTraffic>>,
    },
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use rstest::rstest;

    use crate::{
        incoming::{test::DummyRedirector, RedirectorTask},
        steal::tls::StealTlsHandlerStore,
        util::path_resolver::InTargetPathResolver,
    };

    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn cleanup_on_dead_channel() {
        let (redirector, mut state, _tx) = DummyRedirector::new();
        let (task, mut handle, _) = RedirectorTask::new(
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
