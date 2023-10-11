#![feature(async_fn_in_trait)]
#![feature(return_position_impl_trait_in_trait)]

use std::{
    collections::{hash_map::Entry, HashMap},
    fmt,
    net::SocketAddr,
    time::Duration,
};

use background_tasks::{BackgroundTasks, TaskSender, TaskUpdate};
use codec::AsyncEncoder;
use mirrord_config::LayerConfig;
use protocol::{NewSessionRequest, ProxyToLayerMessage, SessionId};
use session::{NewSessionStream, ProxySessionError};
use tokio::{
    net::{TcpListener, TcpStream},
    time,
};

use crate::{
    agent_conn::AgentConnectInfo,
    background_tasks::TaskError,
    codec::AsyncDecoder,
    error::{IntProxyError, SessionInitError},
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SessionTaskId(SessionId);

impl fmt::Display for SessionTaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LAYER_SESSION {}", self.0)
    }
}

#[derive(Default)]
struct SessionIdStore {
    next_session_id: SessionId,
    session_tasks: HashMap<SessionId, SessionTaskId>,
}

impl SessionIdStore {
    #[tracing::instrument(level = "trace", skip(self), ret)]
    fn get_ids(&mut self, request: &NewSessionRequest) -> (SessionTaskId, SessionId) {
        let new_session_id = self.next_session_id;
        self.next_session_id += 1;

        let task_id = match request {
            NewSessionRequest::New => SessionTaskId(new_session_id),
            NewSessionRequest::Forked(parent) => {
                let parent_session = self
                    .session_tasks
                    .get(parent)
                    .expect("no task found for parent");
                *parent_session
            }
        };

        (task_id, new_session_id)
    }
}

pub struct IntProxy {
    config: LayerConfig,
    agent_connect_info: Option<AgentConnectInfo>,
    listener: TcpListener,
    sessions: BackgroundTasks<SessionTaskId, (), ProxySessionError>,
    session_txs: HashMap<SessionTaskId, TaskSender<NewSessionStream>>,
    session_id_store: SessionIdStore,
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
            sessions: Default::default(),
            session_txs: Default::default(),
            session_id_store: Default::default(),
        }
    }

    #[tracing::instrument(level = "trace", skip(self, stream), ret)]
    async fn handle_new_layer(
        &mut self,
        stream: TcpStream,
        peer: SocketAddr,
    ) -> Result<(), SessionInitError> {
        let mut decoder: AsyncDecoder<LocalMessage<LayerToProxyMessage>, _> =
            AsyncDecoder::new(stream);
        let msg = decoder.receive().await?;

        let Some(msg) = msg else {
            tracing::trace!("layer peer {peer} closed connection without any message");
            return Ok(());
        };

        let new_session_req = match msg.inner {
            LayerToProxyMessage::NewSession(req) => req,
            other => return Err(SessionInitError::UnexpectedMessage(other)),
        };

        let (task_id, new_session_id) = self.session_id_store.get_ids(&new_session_req);

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

        match self.session_txs.entry(task_id) {
            Entry::Occupied(e) => {
                e.get().send(NewSessionStream(stream, new_session_id)).await;
            }
            Entry::Vacant(e) => {
                let session =
                    ProxySession::new(&self.config, self.agent_connect_info.as_ref()).await?;
                let tx = self.sessions.register(
                    session,
                    SessionTaskId(new_session_id),
                    Self::CHANNEL_SIZE,
                );
                e.insert(tx);
            }
        }

        Ok(())
    }

    pub async fn run(&mut self) -> Result<(), IntProxyError> {
        let first_timeout = Duration::from_secs(self.config.internal_proxy.start_idle_timeout);
        let consecutive_timeout = Duration::from_secs(self.config.internal_proxy.idle_timeout);

        let mut any_connection_accepted = false;

        loop {
            tokio::select! {
                res = self.listener.accept() => match res {
                    Ok((stream, peer)) => {
                        self.handle_new_layer(stream, peer).await.map_err(|e| IntProxyError::SessionInitError(peer, e))?;
                        any_connection_accepted = true;
                    },
                    Err(e) => break Err(IntProxyError::AcceptFailed(e)),
                },

                Some((task_id, TaskUpdate::Finished(res))) = self.sessions.next() => match res {
                    Ok(()) => tracing::trace!("{task_id} finished"),
                    Err(TaskError::Error(e)) => {
                        tracing::error!("{task_id} task failed: {e}");
                        return Err(IntProxyError::ProxySessionError(task_id.0, e));
                    }
                    Err(TaskError::Panic) => {
                        tracing::error!("{task_id} task panicked");
                        return Err(IntProxyError::ProxySessionPanic(task_id.0));
                    }
                },

                _ = time::sleep(first_timeout), if !any_connection_accepted => {
                    if !any_connection_accepted {
                        return Err(IntProxyError::FirstConnectionTimeout);
                    }
                },

                _ = time::sleep(consecutive_timeout), if any_connection_accepted && self.session_txs.is_empty() => {
                    if self.session_txs.is_empty() {
                        tracing::trace!("intproxy timeout, no active connections. Exiting.");
                        break Ok(());
                    }
                },
            }
        }
    }
}
