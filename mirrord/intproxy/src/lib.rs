#![feature(async_fn_in_trait)]
#![feature(return_position_impl_trait_in_trait)]

use std::{collections::HashMap, net::SocketAddr, time::Duration};

use background_tasks::{BackgroundTasks, TaskSender, TaskUpdate};
use codec::AsyncEncoder;
use mirrord_config::LayerConfig;
use protocol::{NewSessionRequest, ProxyToLayerMessage, SessionId};
use session::NewSessionStream;
use tokio::{
    net::{TcpListener, TcpStream},
    time,
};

use crate::{
    agent_conn::AgentConnectInfo,
    background_tasks::TaskError,
    codec::AsyncDecoder,
    error::{IntProxyError, Result},
    protocol::{LayerToProxyMessage, LocalMessage},
    session::ProxySession,
};

pub mod agent_conn;
mod background_tasks;
pub mod codec;
pub mod error;
mod layer_conn;
mod ping_pong;
pub mod protocol;
mod proxies;
mod request_queue;
mod session;

pub struct IntProxy {
    config: LayerConfig,
    agent_connect_info: Option<AgentConnectInfo>,
    listener: TcpListener,
    root_sessions: HashMap<SessionId, SessionId>,
    sessions: BackgroundTasks<SessionId, (), IntProxyError>,
    session_txs: HashMap<SessionId, TaskSender<NewSessionStream>>,
    next_session_id: SessionId,
}

impl IntProxy {
    const CHANNEL_SIZE: usize = 512;

    pub fn new(
        config: LayerConfig,
        agent_connect_info: Option<AgentConnectInfo>,
        listener: TcpListener,
    ) -> Self {
        Self {
            config,
            agent_connect_info,
            listener,
            root_sessions: Default::default(),
            sessions: Default::default(),
            session_txs: Default::default(),
            next_session_id: 0,
        }
    }

    #[tracing::instrument(level = "trace", skip(self, stream), ret)]
    async fn handle_new_layer(&mut self, stream: TcpStream, peer: SocketAddr) -> Result<()> {
        let mut decoder: AsyncDecoder<LocalMessage<LayerToProxyMessage>, _> =
            AsyncDecoder::new(stream);
        let msg = decoder.receive().await?;

        let Some(msg) = msg else {
            return Ok(());
        };

        let new_session_req = match msg.inner {
            LayerToProxyMessage::NewSession(req) => req,
            other => return Err(IntProxyError::UnexpectedLayerMessage(other)),
        };

        let new_session_id = self.next_session_id;
        self.next_session_id += 1;

        let mut encoder: AsyncEncoder<LocalMessage<ProxyToLayerMessage>, _> =
            AsyncEncoder::new(decoder.into_inner());
        encoder
            .send(&LocalMessage {
                message_id: msg.message_id,
                inner: ProxyToLayerMessage::NewSession(new_session_id),
            })
            .await?;
        encoder.flush().await?;

        let stream = encoder.into_inner();

        match new_session_req {
            NewSessionRequest::New => {
                let session =
                    ProxySession::new(&self.config, self.agent_connect_info.as_ref()).await?;
                let tx = self
                    .sessions
                    .register(session, new_session_id, Self::CHANNEL_SIZE);
                self.session_txs.insert(new_session_id, tx);
            }
            NewSessionRequest::Forked(parent) => {
                let root = match self.root_sessions.get(&parent) {
                    Some(root) => *root,
                    None => {
                        self.root_sessions.insert(new_session_id, parent);
                        parent
                    }
                };

                let Some(tx) = self.session_txs.get(&root) else {
                    tracing::warn!(
                        "root session {root} of new session {new_session_id} already finished"
                    );
                    return Ok(());
                };

                tx.send(NewSessionStream(stream, new_session_id)).await;
            }
        }

        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
        let first_timeout = Duration::from_secs(self.config.internal_proxy.start_idle_timeout);
        let consecutive_timeout = Duration::from_secs(self.config.internal_proxy.idle_timeout);

        let mut any_connection_accepted = false;

        loop {
            tokio::select! {
                res = self.listener.accept() => match res {
                    Ok((stream, peer)) => {
                        self.handle_new_layer(stream, peer).await?;
                        any_connection_accepted = true;
                    },
                    Err(e) => break Err(IntProxyError::AcceptFailed(e)),
                },

                Some((session_id, TaskUpdate::Finished(res))) = self.sessions.next() => match res {
                    Ok(()) => tracing::trace!("session {session_id} finished"),
                    Err(TaskError::Error(e)) => {
                        tracing::error!("session {session_id} task failed: {e}");
                        return Err(e);
                    }
                    Err(TaskError::Panic) => {
                        tracing::error!("session {session_id} task panicked");
                        return Err(IntProxyError::ProxySessionPanic);
                    }
                },

                _ = time::sleep(first_timeout), if !any_connection_accepted => {
                    if !any_connection_accepted {
                        return Err(IntProxyError::FirstConnectionTimeout);
                    }
                },

                _ = time::sleep(consecutive_timeout), if any_connection_accepted => {
                    if self.session_txs.is_empty() {
                        tracing::trace!("intproxy timeout, no active connections. Exiting.");
                        break Ok(());
                    }

                    tracing::trace!(
                        "intproxy {} sec tick, {} active_connection(s).",
                        self.config.internal_proxy.idle_timeout,
                        self.session_txs.len(),
                    );
                },
            }
        }
    }
}
