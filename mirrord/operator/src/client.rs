use std::io;

use futures::{SinkExt, StreamExt};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::{net::TcpStream, sync::mpsc};

use crate::protocol::{OperatorCodec, OperatorMessage};

pub async fn connect(
    mut client_rx: mpsc::Receiver<ClientMessage>,
) -> io::Result<mpsc::Receiver<DaemonMessage>> {
    let (daemon_tx, daemon_rx) = mpsc::channel(100);

    let connection = TcpStream::connect("127.0.0.1:8080").await?;

    let mut codec = actix_codec::Framed::new(connection, OperatorCodec::new());

    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(Ok(msg)) = codec.next() => {
                     match msg {
                        OperatorMessage::Daemon(msg) => {
                            if let Err(_) = daemon_tx.send(msg).await {
                                println!("DaemonMessage Dropped");
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                Some(client_msg) = client_rx.recv() => {
                    if let Err(_) = codec.send(OperatorMessage::Client(client_msg)).await {
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
