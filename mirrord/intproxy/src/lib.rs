use std::{sync::Arc, time::Duration};

// use file_handler::FileHandler;
use layer_conn::LayerConnection;
use mirrord_config::LayerConfig;
use mirrord_protocol::{ClientMessage, DaemonMessage};
// use protocol::hook::HookMessage;
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time,
};

use crate::{
    agent_conn::{AgentCommunicationFailed, AgentConnectInfo, AgentConnection},
    error::{IntProxyError, Result},
    protocol::{LayerToProxyMessage, LocalMessage},
};

pub mod agent_conn;
pub mod codec;
pub mod error;
mod file_handler;
mod layer_conn;
pub mod protocol;
mod request_queue;

pub struct IntProxy {
    config: LayerConfig,
    agent_connect_info: Option<AgentConnectInfo>,
    listener: TcpListener,
}

impl IntProxy {
    pub fn new(
        config: LayerConfig,
        agent_connect_info: Option<AgentConnectInfo>,
        listener: TcpListener,
    ) -> Self {
        Self {
            config,
            agent_connect_info,
            listener,
        }
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        let first_timeout = Duration::from_secs(self.config.internal_proxy.start_idle_timeout);
        let consecutive_timeout = Duration::from_secs(self.config.internal_proxy.idle_timeout);

        let mut any_connection_accepted = false;

        let mut active_connections = JoinSet::new();

        loop {
            tokio::select! {
                res = self.listener.accept() => match res {
                    Ok((stream, peer)) => {
                        tracing::trace!("accepted connection from {peer}");

                        any_connection_accepted = true;


                        let proxy_clone = self.clone();
                        active_connections.spawn(async move {
                            let session = match ProxySession::new(&proxy_clone, stream).await {
                                Ok(session) => session,
                                Err(err) => {
                                    tracing::error!("failed to create a new session for {peer}: {err:?}");
                                    return;
                                }
                            };

                            if let Err(err) = session.serve_connection().await {
                                tracing::error!("an error occurred when handling connection from {peer}: {err:?}");
                            }
                        });
                    },
                    Err(err) if any_connection_accepted => {
                        tracing::error!("failed to accept connection: {err:#?}");
                        return Err(IntProxyError::AcceptFailed(err));
                    }
                    Err(err) => {
                        tracing::error!("failed to accept first connection: {err:#?}");
                        return Err(IntProxyError::AcceptFailed(err));
                    }
                },

                _ = active_connections.join_next(), if !active_connections.is_empty() => {},

                _ = time::sleep(first_timeout), if !any_connection_accepted => {
                    return Err(IntProxyError::FirstConnectionTimeout);
                },

                _ = time::sleep(consecutive_timeout), if any_connection_accepted => {
                    if active_connections.is_empty() {
                        tracing::trace!("intproxy timeout, no active connections. Exiting.");
                        break Ok(());
                    }

                    tracing::trace!(
                        "intproxy {} sec tick, {} active_connection(s).",
                        self.config.internal_proxy.idle_timeout,
                        active_connections.len(),
                    );
                },
            }
        }
    }
}

struct ProxySession {
    agent_conn: AgentConnection,
    layer_conn: LayerConnection,
    ping: bool,
    // file_handler: FileHandler,
}

impl ProxySession {
    const PING_INTERVAL: Duration = Duration::from_secs(30);

    async fn new(intproxy: &IntProxy, conn: TcpStream) -> Result<Self> {
        let mut agent_conn =
            AgentConnection::new(&intproxy.config, intproxy.agent_connect_info.as_ref()).await?;
        agent_conn.ping_pong().await?;

        let layer_conn = LayerConnection::new(conn);

        // let file_handler =
        //     FileHandler::new(agent_conn.sender().clone(), layer_conn.sender().clone());

        Ok(Self {
            agent_conn,
            layer_conn,
            ping: false,
            // file_handler,
        })
    }

    async fn serve_connection(mut self) -> Result<()> {
        let mut ping_interval = time::interval(Self::PING_INTERVAL);
        ping_interval.tick().await;

        loop {
            tokio::select! {
                layer_message = self.layer_conn.receive() => {
                    ping_interval.reset();

                    let Some(message) = layer_message else {
                        tracing::trace!("layer connection closed");
                        break Ok(());
                    };

                    self.handle_layer_message(message).await?;
                },

                agent_message = self.agent_conn.receive() => {
                    match agent_message {
                        Some(agent_message) => self.handle_agent_message(agent_message).await?,
                        None => {
                            tracing::trace!("agent connection closed");
                            break Ok(());
                        }
                    }
                },

                _ = ping_interval.tick() => {
                    if !self.ping {
                        self.agent_conn.sender().send(ClientMessage::Ping).await?;
                        self.ping = true;
                    } else {
                        tracing::warn!("Unmatched ping, timeout!");
                        break Err(AgentCommunicationFailed::UnmatchedPing.into());
                    }
                }
            }
        }
    }

    async fn handle_agent_message(&mut self, message: DaemonMessage) -> Result<()> {
        match message {
            DaemonMessage::Pong => {
                self.ping = false;
            }
            // DaemonMessage::File(file) => self.file_handler.handle_daemon_message(file).await?,
            _ => todo!(),
        }

        Ok(())
    }

    async fn handle_layer_message(
        &mut self,
        message: LocalMessage<LayerToProxyMessage>,
    ) -> Result<()> {
        match message.inner {
            // LayerToProxyMessage::HookMessage(HookMessage::File(file)) => {
            //     self.file_handler
            //         .handle_hook_message(message.message_id, file)
            //         .await
            // }
            // LayerToProxyMessage::HookMessage(HookMessage::Tcp(_)) => todo!(),
            other => Err(IntProxyError::UnexpectedLayerMessage(other)),
        }
    }
}
