use std::hash::Hash;

use actix_codec::{AsyncRead, AsyncWrite, Framed, FramedParts};
use futures::{SinkExt, StreamExt};
use mirrord_config::LayerConfig;
use mirrord_progress::Progress;
use mirrord_protocol::{
    ClientCodec, ClientMessage, DaemonMessage, DaemonMessageV1, LogLevel, ProtocolCodec,
    VersionCodec,
};
use tokio::{net::TcpStream, sync::mpsc};
use tracing::{error, info, warn};

use crate::error::Result;

pub mod container;
pub mod kubernetes;
mod runtime;

const CONNECTION_CHANNEL_SIZE: usize = 1000;

/// Creates the task that handles the messaging between layer/agent.
/// It does the encoding/decoding of protocol.
pub fn wrap_raw_connection<I, O: DaemonMessage>(
    stream: Framed<TcpStream, VersionCodec>,
) -> (mpsc::Sender<I>, mpsc::Receiver<O>) {
    let framed_parts = stream.into_parts();
    let framed_parts = FramedParts::with_read_buf(
        framed_parts.io,
        ProtocolCodec::<I, O>::default(),
        framed_parts.read_buf,
    );
    let mut codec = Framed::from_parts(framed_parts);
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
                    if let Some(log_message) = daemon_message.as_log_message() {
                            match log_message.level {
                                LogLevel::Warn => {
                                    warn!(message = log_message.message, "Daemon sent log message")
                                }
                                LogLevel::Error => {
                                    error!(message = log_message.message, "Daemon sent log message")
                                }
                            }
                    } else {
                        match daemon_message {
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
        }
    });

    (in_tx, out_rx)
}

pub trait AgentManagment {
    type AgentRef: Hash + Eq;
    type Err;

    async fn connect<P>(
        &self,
        progress: &mut P,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessageV1>), Self::Err>
    where
        P: Progress + Send + Sync,
        Self::AgentRef: Send + Sync,
        Self::Err: Send + Sync,
    {
        self.create_connection(self.create_agent(progress, None).await?)
            .await
    }

    async fn create_connection(
        &self,
        agent_ref: Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessageV1>), Self::Err>;

    async fn create_agent<P>(
        &self,
        progress: &mut P,
        config: Option<&LayerConfig>,
    ) -> Result<Self::AgentRef, Self::Err>
    where
        P: Progress + Send + Sync;
}
