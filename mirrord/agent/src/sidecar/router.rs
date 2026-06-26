use std::{future::Future, io, pin::Pin, sync::Arc, time::Duration};

use mirrord_protocol_io::{Agent, Connection};
use tokio::{net::TcpListener, sync::mpsc, task::JoinSet, time::timeout};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{AgentError, AgentResult},
    sidecar::{BridgeIngressTx, SidecarIntProxyBridge},
};

pub type SpawnClientSession = Arc<
    dyn Fn(Connection<Agent>) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> + Send + Sync,
>;

/// Supervises remote bridge sessions and upstream `Connection<Agent>` sessions.
pub struct SidecarRouter {
    bridge_listener: TcpListener,
    upstream_rx: mpsc::UnboundedReceiver<Connection<Agent>>,
    bridge_ingress_tx: BridgeIngressTx,
    spawn_client_session: SpawnClientSession,
    cancellation_token: CancellationToken,
    bridge_timeout: Duration,
    join_set: JoinSet<()>,
}

impl SidecarRouter {
    pub fn new(
        bridge_listener: TcpListener,
        upstream_rx: mpsc::UnboundedReceiver<Connection<Agent>>,
        bridge_ingress_tx: BridgeIngressTx,
        spawn_client_session: SpawnClientSession,
        cancellation_token: CancellationToken,
        bridge_timeout: Duration,
    ) -> Self {
        Self {
            bridge_listener,
            upstream_rx,
            bridge_ingress_tx,
            spawn_client_session,
            cancellation_token,
            bridge_timeout,
            join_set: JoinSet::new(),
        }
    }

    pub async fn run(mut self) -> AgentResult<()> {
        let bridge_listener = &mut self.bridge_listener;
        let upstream_rx = &mut self.upstream_rx;
        let bridge_ingress_tx = self.bridge_ingress_tx.clone();
        let spawn_client_session = self.spawn_client_session.clone();
        let cancellation_token = self.cancellation_token.clone();
        let bridge_timeout = self.bridge_timeout;
        let join_set = &mut self.join_set;
        let mut seen_bridge = false;
        let mut bridge_closed = false;
        let mut upstream_closed = false;

        loop {
            if bridge_closed && upstream_closed && join_set.is_empty() {
                return Ok(());
            }

            // The router waits on three independent sources of progress:
            // - cancellation from the agent shutdown path,
            // - completion of child tasks we spawned for either incoming source,
            // - new upstream sessions-manager connections, which represent layer-local sessions,
            // - new bridge sockets, which represent the `intproxy-remote` sidecar path.
            tokio::select! {
                // Agent shutdown or explicit cancellation: stop supervising everything and exit.
                _ = cancellation_token.cancelled() => {
                    join_set.abort_all();
                    return Ok(());
                }

                // A child task finished. These tasks can come from either source:
                // - an upstream `Connection<Agent>` spawned for a layer-local session from
                //   sessions-manager,
                // - a `SidecarIntProxyBridge` spawned for the remote `intproxy-remote` bridge.
                joined = join_set.join_next(), if !join_set.is_empty() => {
                    match joined {
                        Some(Ok(())) => {}
                        Some(Err(error)) => {
                            if error.is_panic() {
                                tracing::error!(%error, "sidecar child task panicked");
                            } else {
                                tracing::error!(%error, "sidecar child task failed to join");
                            }
                        }
                        None => {}
                    }
                }

                // A new upstream session-manager connection arrived. This is the layer-local
                // path: the agent should start a regular `ClientConnectionHandler` for it.
                maybe_upstream = upstream_rx.recv(), if !upstream_closed => {
                    match maybe_upstream {
                        Some(connection) => {
                            let spawn_client_session = Arc::clone(&spawn_client_session);
                            join_set.spawn(async move {
                                (spawn_client_session)(connection).await;
                            });
                        }
                        None => {
                            upstream_closed = true;
                        }
                    }
                }

                // A new bridge socket arrived. This is the remote-sidecar path: the accepted
                // socket belongs to `intproxy-remote`, which will translate remote incoming intent
                // into bridged traffic for the shared incoming pipeline.
                bridge_result = async {
                    if seen_bridge {
                        bridge_listener.accept().await.map(Some)
                    } else {
                        match timeout(bridge_timeout, bridge_listener.accept()).await {
                            Ok(result) => result.map(Some),
                            Err(_) => Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "timed out waiting for the first sidecar bridge connection",
                            )),
                        }
                    }
                }, if !bridge_closed => {
                    match bridge_result {
                        Ok(Some((stream, peer))) => {
                            seen_bridge = true;
                            tracing::trace!(peer = %peer, "accepted sidecar bridge connection");
                            let ingress_tx = bridge_ingress_tx.clone();
                            let bridge_listener_addr = bridge_listener.local_addr().unwrap();
                            join_set.spawn(async move {
                                match SidecarIntProxyBridge::new(
                                    stream,
                                    bridge_listener_addr,
                                    ingress_tx,
                                ) {
                                    Ok(bridge) => {
                                        if let Err(error) = bridge.run().await {
                                            tracing::error!(%error, "sidecar intproxy bridge failed");
                                        }
                                    }
                                    Err(error) => {
                                        tracing::error!(%error, "failed to initialize sidecar intproxy bridge");
                                    }
                                }
                            });
                        }
                        Ok(None) => {
                            bridge_closed = true;
                        }
                        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                            return Err(AgentError::FirstConnectionTimeout);
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
            }
        }
    }
}
