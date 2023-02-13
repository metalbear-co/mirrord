//! Module for the mirrord-layer/mirrord-agent connection mechanism.
//!
//! The layer will either start a new agent pod with [`KubernetesAPI`], directly connect to an
//! existing agent (currently only used for tests), or let the [`OperatorApi`] handle the
//! connection.
use std::{net::SocketAddr, time::Duration};

use mirrord_config::LayerConfig;
use mirrord_kube::{
    api::{kubernetes::KubernetesAPI, AgentManagment, Connection},
    error::KubeApiError,
};
use mirrord_operator::client::{OperatorApi, OperatorApiError};
use mirrord_progress::NoProgress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, Sender},
};
use tracing::log::info;

use crate::{graceful_exit, FAIL_STILL_STUCK};



const GENERAL_HELP: &str = "Please join our discord https://discord.com/invite/J5YSrStDKD and request help in #mirrord-help.";


/// Calls [`graceful_exit`] when we have an [`OperatorApiError`].
///
/// Used in [`connect`] (same as [`handle_error`]), but only when `LayerConfig::operator` is
/// set to `true` and an operator is installed in the cluster.
fn handle_operator_error(err: OperatorApiError) -> ! {
    graceful_exit!("{FAIL_CREATE_AGENT}{FAIL_STILL_STUCK} with error {err}")
}

/// Connects to the internal proxy in given `SocketAddr`
/// layer uses to communicate with it, in the form of a [`Sender`] for [`ClientMessage`]s, and a
/// [`Receiver`] for [`DaemonMessage`]s.
pub(crate) async fn connect(addr: SocketAddr) -> (Sender<ClientMessage>, Receiver<DaemonMessage>) {
    let progress = NoProgress;
    let stream = TcpStream::connect(addr).await.expect(concat!(
        "Couldn't connect to internal proxy, please join the discord server and report this bug",
        GENERAL_HELP
    ));
    wrap_raw_connection(stream).expect(concat!("Couldn't wrap raw connection, ", GENERAL_HELP))
}

fn wrap_raw_connection(
    stream: TcpStream,
) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
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

    Ok((in_tx, out_rx))
}
