use actix_codec::{AsyncRead, AsyncWrite};
use futures::{SinkExt, StreamExt};
use k8s_openapi::{api::core::v1::Namespace, NamespaceResourceScope};
use kube::{api::ListParams, Api, Client};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage, LogLevel};
use tokio::{
    net::{TcpStream, ToSocketAddrs},
    sync::mpsc,
};
use tracing::{debug, error, info, warn};

use crate::error::{KubeApiError, Result};

pub mod container;
pub mod kubernetes;
mod runtime;

const CONNECTION_CHANNEL_SIZE: usize = 1000;

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

/// Get a vector of namespaces from an optional namespace. If the given namespace is Some, then
/// fetch its Namespace object, and return a vector only with that.
/// If the namespace is None - return all namespaces.
pub async fn get_namespaces(
    client: &Client,
    namespace: Option<&str>,
    lp: &ListParams,
) -> Result<Vec<Namespace>> {
    let api: Api<Namespace> = Api::all(client.clone());
    Ok(if let Some(namespace) = namespace {
        vec![api.get(namespace).await?]
    } else {
        api.list(lp).await?.items
    })
}

/// Check if the client can see a given namespace.
pub async fn namespace_exists_for_client(namespace: &str, client: &Client) -> bool {
    let api: Api<Namespace> = Api::all(client.clone());
    api.get(namespace).await.is_ok()
}

/// Creates the task that handles the messaging between layer/agent.
/// It does the encoding/decoding of protocol.
pub fn wrap_raw_connection(
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
) -> (mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>) {
    debug!("wrap_raw_connection");
    let mut codec = actix_codec::Framed::new(stream, ClientCodec::new());

    let (in_tx, mut in_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);
    let (out_tx, out_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);

    tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = in_rx.recv() => {
                    debug!("next layer message: {msg:?}");
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
                    debug!("next daemon message: {daemon_message:?}");
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
                }
            }
        }
    });

    (in_tx, out_rx)
}

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

pub struct Connection<T: ToSocketAddrs>(pub T); // TODO: Replace with generic address

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
        Ok(wrap_raw_connection(stream))
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
