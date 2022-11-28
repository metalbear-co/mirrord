use std::io;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use mirrord_config::target::TargetConfig;
use mirrord_kube::api::AgentManagment;
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::{
    net::{TcpStream, ToSocketAddrs},
    sync::mpsc,
};

use crate::protocol::{Handshake, OperatorCodec, OperatorRequest, OperatorResponse};

pub struct OperatorApi<T: ToSocketAddrs> {
    addr: T,
    target: TargetConfig,
}

impl<T> OperatorApi<T>
where
    T: ToSocketAddrs,
{
    pub fn new(addr: T, target: TargetConfig) -> Self {
        OperatorApi { addr, target }
    }
}

#[async_trait]
impl<T> AgentManagment for OperatorApi<T>
where
    T: ToSocketAddrs + Send + Sync,
{
    type AgentRef = actix_codec::Framed<TcpStream, OperatorCodec>;
    type Err = io::Error;

    async fn create_connection(
        &self,
        mut codec: Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), Self::Err> {
        let (client_tx, mut client_rx) = mpsc::channel(100);
        let (daemon_tx, daemon_rx) = mpsc::channel(100);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(Ok(msg)) = codec.next() => {
                         match msg {
                            OperatorResponse::Daemon(msg) => {
                                if daemon_tx.send(msg).await.is_err() {
                                    println!("DaemonMessage Dropped");
                                    break;
                                }
                            }
                        }
                    }
                    Some(client_msg) = client_rx.recv() => {
                        if codec.send(OperatorRequest::Client(client_msg)).await.is_err() {
                            println!("DaemonMessage Dropped");
                            break;
                        }
                    }
                    else => { break }
                }
            }
        });

        Ok((client_tx, daemon_rx))
    }

    async fn create_agent<P>(&self, _: &P) -> Result<Self::AgentRef, Self::Err>
    where
        P: Progress + Send + Sync,
    {
        let connection = TcpStream::connect(&self.addr).await?;

        let mut codec = actix_codec::Framed::new(connection, OperatorCodec::client());

        let _ = codec
            .send(OperatorRequest::Handshake(Handshake::new(
                self.target.clone(),
            )))
            .await;

        Ok(codec)
    }
}
