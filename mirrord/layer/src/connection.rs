//! Module for the mirrord-layer/mirrord-agent connection mechanism.
//!
//! The layer will connect to the internal proxy to communicate with the agent.
use std::net::SocketAddr;

use futures::{SinkExt, StreamExt};
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage, LogLevel};
use tokio::{net::TcpStream, sync::mpsc};
use tracing::{error, info, trace, warn};

use crate::graceful_exit;

/// Size of the channel used in [`wrap_raw_connection`] for [`ClientMessage`]s.
const CONNECTION_CHANNEL_SIZE: usize = 1000;

const FAIL_STILL_STUCK: &str = r#"
- If you're still stuck:

>> Please open a new bug report at https://github.com/metalbear-co/mirrord/issues/new/choose

>> Or join our Discord https://discord.gg/metalbear and request help in #mirrord-help

>> Or email us at hi@metalbear.co

"#;

/// Connects to the internal proxy in given `SocketAddr`
/// layer uses to communicate with it, in the form of a [`Sender`] for [`ClientMessage`]s, and a
/// [`Receiver`] for [`DaemonMessage`]s.
/// Creates the task that handles the messaging between layer/agent.
/// It does the encoding/decoding of protocol.
/// We connect in a separate task to avoid blocking the execution of the real process until the
/// connection happens. Should help in stressful situations like https://github.com/metalbear-co/mirrord/issues/1716
pub(crate) async fn connect_to_proxy(
    addr: SocketAddr,
) -> (mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>) {
    let (client_in_tx, mut client_in_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);
    let (daemon_out_tx, daemon_out_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);

    tokio::spawn(async move {
        let stream = match TcpStream::connect(addr).await {
            Ok(stream) => stream,
            Err(e) => {
                error!("Couldn't connect to internal proxy: {e:?}, {addr:?}");
                graceful_exit!("{FAIL_STILL_STUCK:?}");
            }
        };
        let mut codec = actix_codec::Framed::new(stream, ClientCodec::new());

        loop {
            tokio::select! {
                msg = client_in_rx.recv() => {
                    match msg {
                        Some(msg) => {
                            trace!("Sending client message: {:?}", &msg);
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
                        },
                        Some(Ok(msg)) => {
                            if let Err(fail) = daemon_out_tx.send(msg).await {
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

    (client_in_tx, daemon_out_rx)
}
