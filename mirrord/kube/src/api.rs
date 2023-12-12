use std::hash::Hash;

use actix_codec::{AsyncRead, AsyncWrite};
use futures::{SinkExt, StreamExt};
use mirrord_config::LayerConfig;
use mirrord_progress::Progress;
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage, LogLevel};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::error::Result;

pub mod container;
pub mod kubernetes;
mod runtime;

const CONNECTION_CHANNEL_SIZE: usize = 1000;

/// Creates the task that handles the messaging between layer/agent.
/// It does the encoding/decoding of protocol.
pub fn wrap_raw_connection(
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
) -> (mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>) {
    let mut codec = actix_codec::Framed::new(stream, ClientCodec::default());

    let (in_tx, mut in_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);
    let (out_tx, out_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);

    tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = in_rx.recv() => {
                    match msg {
                        Some(msg) => {
                            if let Err(fail) = codec.send(msg).await {
                                error!("Error sending client message: {:#?}", fail);
                                break;
                            }
                        }
                        None => {
                            info!("mirrord-kube: initiated disconnect from agent");

                            break;
                        }
                    }
                }
                daemon_message = codec.next() => {
                    match daemon_message {
                        Some(Ok(DaemonMessage::LogMessage(log_message))) => {
                            match log_message.level {
                                LogLevel::Warn => {
                                    warn!(message = log_message.message, "Daemon sent log message")
                                }
                                LogLevel::Error => {
                                    error!(message = log_message.message, "Daemon sent log message")
                                }
                            }
                        }
                        Some(Ok(msg)) => {
                            if let Err(fail) = out_tx.send(msg).await {
                                error!("DaemonMessage dropped: {:#?}", fail);

                                break;
                            }
                        }
                        Some(Err(err)) => {
                            error!("Error receiving daemon message: {:?}", err);
                            break;
                        }
                        None => {
                            info!("agent disconnected");

                            break;
                        }
                    }
                }
            }
        }
    });

    (in_tx, out_rx)
}

pub trait AgentManagment {
    type AgentRef: Hash + Eq;
    type Err;

    #[allow(async_fn_in_trait)]
    async fn connect<P>(
        &self,
        progress: &mut P,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), Self::Err>
    where
        P: Progress + Send + Sync,
        Self::AgentRef: Send + Sync,
        Self::Err: Send + Sync,
    {
        self.create_connection(self.create_agent(progress, None).await?)
            .await
    }

    #[allow(async_fn_in_trait)]
    async fn create_connection(
        &self,
        agent_ref: Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), Self::Err>;

    #[allow(async_fn_in_trait)]
    async fn create_agent<P>(
        &self,
        progress: &mut P,
        config: Option<&LayerConfig>,
    ) -> Result<Self::AgentRef, Self::Err>
    where
        P: Progress + Send + Sync;
}
