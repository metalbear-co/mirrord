use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use http::request::Request;
use kube::{Api, Client};
use mirrord_config::{target::TargetConfig, LayerConfig};
use mirrord_kube::{
    api::{get_k8s_resource_api, kubernetes::create_kube_api, AgentManagment},
    error::KubeApiError,
};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};

use crate::crd::TargetCrd;

static CONNECTION_CHANNEL_SIZE: usize = 1000;

#[derive(Debug, Error)]
pub enum OperatorApiError {
    #[error("unable to create target for TargetConfig")]
    InvalidTarget,
    #[error(transparent)]
    HttpError(#[from] http::Error),
    #[error(transparent)]
    KubeApiError(#[from] KubeApiError),
}

type Result<T, E = OperatorApiError> = std::result::Result<T, E>;

pub struct OperatorApi {
    client: Client,
    target: TargetConfig,
}

impl OperatorApi {
    pub async fn discover<P>(config: &LayerConfig, progress: &P) -> Result<(Self, TargetCrd)>
    where
        P: Progress + Send + Sync,
    {
        let operator_api = OperatorApi::new(&config).await?;

        let operator_ref = operator_api.create_agent(progress).await?;

        Ok((operator_api, operator_ref))
    }

    pub async fn new(config: &LayerConfig) -> Result<Self> {
        let target = config.target.clone();

        let client = create_kube_api(Some(config.clone())).await?;

        Ok(OperatorApi { client, target })
    }
}

#[async_trait]
impl AgentManagment for OperatorApi {
    type AgentRef = TargetCrd;
    type Err = OperatorApiError;

    async fn create_connection(
        &self,
        target: Self::AgentRef,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        let target_api: Api<TargetCrd> =
            get_k8s_resource_api(&self.client, self.target.namespace.as_deref());

        let connection = self
            .client
            .connect(
                Request::builder()
                    .uri(format!(
                        "{}/{}?connect=true",
                        target_api.resource_url(),
                        target.name()
                    ))
                    .body(vec![])?,
            )
            .await
            .map_err(KubeApiError::from)?;

        wrap_connection(connection)
    }

    async fn create_agent<P>(&self, _: &P) -> Result<Self::AgentRef>
    where
        P: Progress + Send + Sync,
    {
        let target_api: Api<TargetCrd> =
            get_k8s_resource_api(&self.client, self.target.namespace.as_deref());

        let target = self
            .target
            .path
            .as_ref()
            .ok_or(OperatorApiError::InvalidTarget)?;

        target_api
            .get(&TargetCrd::target_name(target))
            .await
            .map_err(KubeApiError::from)
            .map_err(OperatorApiError::from)
    }
}

fn wrap_connection<T>(
    mut connection: T,
) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)>
where
    for<'stream> T: StreamExt<Item = Result<Message, TungsteniteError>>
        + SinkExt<Message, Error = TungsteniteError>
        + Send
        + Unpin
        + 'stream,
{
    let (client_tx, mut client_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);
    let (daemon_tx, daemon_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);

    tokio::spawn(async move {
        loop {
            tokio::select! {
                client_message = client_rx.recv() => {
                    if let Some(client_message) = client_message {
                        if let Ok(payload) = bincode::encode_to_vec(client_message, bincode::config::standard()) {
                            if let Err(err) = connection.send(payload.into()).await {
                                println!("{:?}", err);

                                break;
                            }
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                daemon_message = connection.next() => {
                    if let Some(Ok(Message::Binary(payload))) = daemon_message {
                        if let Ok((daemon_message, _)) = bincode::decode_from_slice::<DaemonMessage, _>(&payload, bincode::config::standard()) {
                            if let Err(err) = daemon_tx.send(daemon_message.into()).await {
                                println!("{:?}", err);

                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
            }
        }

        let _ = connection.send(Message::Close(None)).await;
    });

    Ok((client_tx, daemon_rx))
}

#[cfg(test)]
mod tests {

    use mirrord_progress::NoProgress;

    use super::*;

    #[tokio::test]
    async fn connection() {
        let mut config = LayerConfig::from_env().unwrap();

        config.target.path = "deploy/py-serv-deployment".parse().ok();

        let api = OperatorApi::new(&config).await.unwrap();

        let target = api.create_agent(&NoProgress).await.unwrap();

        let (client_tx, mut daemon_rx) = api.create_connection(target).await.unwrap();

        let _ = client_tx.send(ClientMessage::Ping).await;

        println!("{:#?}", daemon_rx.recv().await);
    }
}
