use std::io;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use mirrord_config::{agent::AgentConfig, target::TargetConfig};
use mirrord_kube::api::AgentManagment;
use mirrord_progress::TaskProgress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::{
    net::{TcpStream, ToSocketAddrs},
    sync::mpsc,
};

use crate::protocol::{AgentInitialize, OperatorCodec, OperatorRequest, OperatorResponse};

pub struct OperatorApi<T: ToSocketAddrs = &'static str> {
    addr: T,
    agent: AgentConfig,
    target: TargetConfig,
}

impl OperatorApi {
    pub fn new(agent: AgentConfig, target: TargetConfig) -> Self {
        OperatorApi {
            addr: "127.0.0.1:8080",
            agent,
            target,
        }
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

    async fn create_agent(&self, _: &TaskProgress) -> Result<Self::AgentRef, Self::Err> {
        let connection = TcpStream::connect(&self.addr).await?;

        let mut codec = actix_codec::Framed::new(connection, OperatorCodec::client());

        let _ = codec
            .send(OperatorRequest::Initialize(AgentInitialize {
                agent: self.agent.clone(),
                target: self.target.clone(),
            }))
            .await;

        Ok(codec)
    }
}

#[cfg(test)]
mod tests {

    use mirrord_config::{
        config::MirrordConfig, target::TargetFileConfig, LayerConfig, LayerFileConfig,
    };

    use super::*;

    #[tokio::test]
    async fn simple() -> anyhow::Result<()> {
        let LayerConfig { agent, target, .. } = LayerFileConfig {
            target: Some(TargetFileConfig::Simple(
                "deploy/py-serv-deployment".parse().ok(),
            )),
            ..Default::default()
        }
        .generate_config()
        .unwrap();

        let api = OperatorApi::new(agent, target);

        let progress = TaskProgress::new("starting operator controlled agent");
        let agent_ref = api.create_agent(&progress).await?;

        let (client_tx, mut daemon_rx) = api.create_connection(agent_ref).await?;

        client_tx.send(ClientMessage::Ping).await?;

        println!("{:?}", daemon_rx.recv().await);

        Ok(())
    }
}
