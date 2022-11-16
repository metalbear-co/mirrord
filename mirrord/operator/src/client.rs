use std::io;

use futures::{SinkExt, StreamExt};
use mirrord_config::LayerConfig;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::{net::TcpStream, sync::mpsc};

use crate::protocol::{AgentInitialize, OperatorCodec, OperatorRequest, OperatorResponse};

pub async fn connect(
    config: &LayerConfig,
    mut client_rx: mpsc::Receiver<ClientMessage>,
) -> io::Result<mpsc::Receiver<DaemonMessage>> {
    let (daemon_tx, daemon_rx) = mpsc::channel(100);

    let connection = TcpStream::connect("127.0.0.1:8080").await?;

    let mut codec = actix_codec::Framed::new(connection, OperatorCodec::client());

    let _ = codec
        .send(OperatorRequest::Initialize(AgentInitialize {
            agent: config.agent.clone(),
            target: config.target.clone(),
        }))
        .await;

    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(Ok(msg)) = codec.next() => {
                     match msg {
                        OperatorResponse::Daemon(msg) => {
                            if let Err(_) = daemon_tx.send(msg).await {
                                println!("DaemonMessage Dropped");
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                Some(client_msg) = client_rx.recv() => {
                    if let Err(_) = codec.send(OperatorRequest::Client(client_msg)).await {
                        println!("DaemonMessage Dropped");
                        break;
                    }
                }
                else => { break }
            }
        }
    });

    Ok(daemon_rx)
}

#[cfg(test)]
mod tests {

    use mirrord_config::{config::MirrordConfig, LayerFileConfig};

    use super::*;

    #[tokio::test]
    async fn simple() -> anyhow::Result<()> {
        let config = LayerFileConfig::default().generate_config().unwrap();

        let (client_tx, client_rx) = mpsc::channel(100);

        let mut daemon_rx = connect(&config, client_rx).await?;

        client_tx.send(ClientMessage::Ping).await?;

        println!("{:?}", daemon_rx.recv().await);

        Ok(())
    }
}
