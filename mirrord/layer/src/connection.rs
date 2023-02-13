//! Module for the mirrord-layer/mirrord-agent connection mechanism.
//!
//! The layer will connect to the internal proxy to communicate with the agent.
use std::{net::SocketAddr, time::Duration};

use futures::{SinkExt, StreamExt};
use mirrord_config::LayerConfig;
use mirrord_kube::{
    api::{kubernetes::KubernetesAPI, AgentManagment, Connection},
    error::KubeApiError,
};
use mirrord_operator::client::{OperatorApi, OperatorApiError};
use mirrord_progress::NoProgress;
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use tokio::{
    net::TcpStream,
    sync::mpsc::{self, Receiver, Sender},
};
use tracing::log::{error, info};

use crate::{graceful_exit, FAIL_STILL_STUCK};

const CONNECTION_CHANNEL_SIZE: usize = 1000;

/// Connects to the internal proxy in given `SocketAddr`
/// layer uses to communicate with it, in the form of a [`Sender`] for [`ClientMessage`]s, and a
/// [`Receiver`] for [`DaemonMessage`]s.
pub(crate) async fn connect(addr: SocketAddr) -> (Sender<ClientMessage>, Receiver<DaemonMessage>) {
    let stream = TcpStream::connect(addr).await.expect(concat!(
        "Couldn't connect to internal proxy, please join the discord server and report this bug ",
        "Please join our discord https://discord.com/invite/J5YSrStDKD and request help in #mirrord-help."
    ));
    wrap_raw_connection(stream)
}

fn wrap_raw_connection(
    stream: TcpStream,
) -> (mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>) {
    let mut codec = actix_codec::Framed::new(stream, ClientCodec::new());

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
                            error!("agent disconnected");

                            break;
                        }
                    }
                }
            }
        }
    });

    (in_tx, out_rx)
}
