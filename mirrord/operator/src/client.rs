use std::io;

use actix_codec::{AsyncRead, AsyncWrite};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::{api::ListParams, Api, Client};
use mirrord_config::{target::TargetConfig, LayerConfig};
use mirrord_kube::{
    api::{get_k8s_api, kubernetes::create_kube_api, AgentManagment},
    error::KubeApiError,
};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::{
    net::{TcpStream, ToSocketAddrs},
    sync::mpsc,
};

use crate::protocol::{Handshake, OperatorCodec, OperatorRequest, OperatorResponse};

static CONNECTION_CHANNEL_SIZE: usize = 1000;

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
        codec: Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), Self::Err> {
        wrap_connection(codec)
    }

    async fn create_agent<P>(&self, _: &P) -> Result<Self::AgentRef, Self::Err>
    where
        P: Progress + Send + Sync,
    {
        let stream = TcpStream::connect(&self.addr).await?;

        Ok(connection(stream, self.target.clone()).await)
    }
}

pub struct OperatorApiDiscover {
    client: Client,
    namespace: String,
    target: TargetConfig,
}

impl OperatorApiDiscover {
    pub async fn create(config: &LayerConfig) -> Result<Self, KubeApiError> {
        let client = create_kube_api(config).await?;

        Ok(OperatorApiDiscover {
            client,
            namespace: "mirrord".to_owned(),
            target: config.target.clone(),
        })
    }

    pub async fn discover_operator<P: Progress + Send + Sync>(
        config: &LayerConfig,
        progress: &P,
    ) -> Option<(Self, <Self as AgentManagment>::AgentRef)> {
        let api = OperatorApiDiscover::create(config).await.ok()?;

        let connection = api.create_agent(progress).await.ok()?;

        Some((api, connection))
    }
}

#[async_trait]
impl AgentManagment for OperatorApiDiscover {
    type AgentRef = String;
    type Err = KubeApiError;

    async fn create_connection(
        &self,
        pod_name: Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), Self::Err> {
        let pod_api: Api<Pod> = get_k8s_api(&self.client, Some(&self.namespace));

        let mut portforwarder = pod_api.portforward(&pod_name, &[8080]).await?;

        let codec = connection(
            portforwarder.take_stream(8080).unwrap(),
            self.target.clone(),
        )
        .await;

        wrap_connection(codec).map_err(KubeApiError::from)
    }

    async fn create_agent<P>(&self, _: &P) -> Result<Self::AgentRef, Self::Err>
    where
        P: Progress + Send + Sync,
    {
        let pod_api: Api<Pod> = get_k8s_api(&self.client, Some(&self.namespace));
        let lp = ListParams::default().labels("app=mirrord-operator");

        let pod_name = pod_api
            .list(&lp)
            .await?
            .items
            .pop()
            .and_then(|pod| pod.metadata.name.clone())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::Other, "Operator Not Found".to_owned())
            })?;

        Ok(pod_name)
    }
}

async fn connection<T>(connection: T, target: TargetConfig) -> actix_codec::Framed<T, OperatorCodec>
where
    for<'con> T: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'con,
{
    let mut codec = actix_codec::Framed::new(connection, OperatorCodec::client());

    let _ = codec
        .send(OperatorRequest::Handshake(Handshake::new(target)))
        .await;

    codec
}

fn wrap_connection<T>(
    mut codec: actix_codec::Framed<T, OperatorCodec>,
) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), std::io::Error>
where
    for<'con> T: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'con,
{
    let (client_tx, mut client_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);
    let (daemon_tx, daemon_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);

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
