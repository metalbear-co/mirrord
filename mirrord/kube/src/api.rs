use actix_codec::{AsyncRead, AsyncWrite};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use k8s_openapi::NamespaceResourceScope;
use kube::{Api, Client};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use tokio::{
    net::{TcpStream, ToSocketAddrs},
    sync::mpsc,
};
use tracing::error;

use crate::error::{KubeApiError, Result};

mod container;
#[cfg(feature = "env_guard")]
mod env_guard;
pub mod kubernetes;
mod runtime;

pub(crate) fn get_k8s_api<K>(client: &Client, namespace: Option<&str>) -> Api<K>
where
    K: kube::Resource<Scope = NamespaceResourceScope>,
    <K as kube::Resource>::DynamicType: Default,
{
    if let Some(namespace) = namespace {
        Api::namespaced(client.clone(), namespace)
    } else {
        Api::default_namespaced(client.clone())
    }
}

pub(crate) fn wrap_raw_connection(
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
    let mut codec = actix_codec::Framed::new(stream, ClientCodec::new());

    let (in_tx, mut in_rx) = mpsc::channel(1000);
    let (out_tx, out_rx) = mpsc::channel(1000);

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
                            error!("initiated disconnect from agent");

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

#[async_trait]
pub trait AgentManagment {
    type AgentRef;
    type Err;

    async fn create_connection(
        &self,
        agent_ref: Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), Self::Err>;

    async fn create_agent<P>(&self, progress: &P) -> Result<Self::AgentRef, Self::Err>
    where
        P: Progress + Send + Sync;
}

pub struct Connection<T: ToSocketAddrs>(pub T); // TODO: Replace with generic address

#[async_trait]
impl<T> AgentManagment for Connection<T>
where
    T: ToSocketAddrs + Send + Sync,
{
    type AgentRef = TcpStream;
    type Err = KubeApiError;

    async fn create_connection(
        &self,
        stream: Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        wrap_raw_connection(stream)
    }

    async fn create_agent<P>(&self, _: &P) -> Result<Self::AgentRef, Self::Err>
    where
        P: Progress + Send + Sync,
    {
        TcpStream::connect(&self.0)
            .await
            .map_err(KubeApiError::from)
    }
}
