use actix_codec::{AsyncRead, AsyncWrite};
use futures::{SinkExt, StreamExt};
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage, LogLevel};
use tokio::sync::mpsc;
use tracing::{error, info, trace, warn};

const CONNECTION_CHANNEL_SIZE: usize = 1000;

/// Creates the task that handles the messaging between layer/agent.
/// It does the encoding/decoding of protocol.
pub fn wrap_raw_connection(
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
) -> (mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>) {
    let mut codec = actix_codec::Framed::new(stream, ClientCodec::new());

    let (in_tx, mut in_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);
    let (out_tx, out_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);

    tokio::spawn(async move {
        let mut timeout_check = tokio::time::interval(std::time::Duration::from_secs(2));
        timeout_check.tick().await;
        let mut ticked = false;
        loop {
            tokio::select! {
                msg = in_rx.recv() => {
                    timeout_check.reset();
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
                        },
                        Some(Ok(DaemonMessage::Pong)) => {
                            ticked = false;
                            trace!("Received pong from agent");
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
                },
                _ = timeout_check.tick() => {
                    if ticked {
                        info!("Timeout waiting for agent message");
                        break
                    } else {
                        if let Err(fail) = codec.send(ClientMessage::Ping).await {
                            error!("Error sending client message: {:#?}", fail);
                            break;
                        }
                        ticked = true;
                    }
                }
            }
        }
    });

    (in_tx, out_rx)
}
