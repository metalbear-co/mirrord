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
use tracing::{error, info};

use crate::error::{KubeApiError, Result};

pub mod container;
pub mod kubernetes;
mod runtime;

static CONNECTION_CHANNEL_SIZE: usize = 1000;

pub fn get_k8s_resource_api<K>(client: &Client, namespace: Option<&str>) -> Api<K>
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


#[async_trait]
pub trait AgentManagment {
    type AgentRef;
    type Err;

    async fn connect<P>(
        &self,
        progress: &P,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), Self::Err>
    where
        P: Progress + Send + Sync,
        Self::AgentRef: Send + Sync,
        Self::Err: Send + Sync,
    {
        self.create_connection(self.create_agent(progress).await?)
            .await
    }

    async fn create_connection(
        &self,
        agent_ref: Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), Self::Err>;

    async fn create_agent<P>(&self, progress: &P) -> Result<Self::AgentRef, Self::Err>
    where
        P: Progress + Send + Sync;
}
