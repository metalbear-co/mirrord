use futures::{SinkExt, StreamExt};
use http::request::Request;
use kube::{error::ErrorResponse, Api, Client};
use mirrord_config::{target::TargetConfig, LayerConfig};
use mirrord_kube::{
    api::{get_k8s_resource_api, kubernetes::create_kube_api},
    error::KubeApiError,
};
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
    target_api: Api<TargetCrd>,
    target_config: TargetConfig,
}

impl OperatorApi {
    pub async fn discover(
        config: &LayerConfig,
    ) -> Result<Option<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)>> {
        let operator_api = OperatorApi::new(config).await?;

        if let Some(target) = operator_api.fetch_target().await? {
            operator_api.connect_target(target).await.map(Some)
        } else {
            Ok(None)
        }
    }

    async fn new(config: &LayerConfig) -> Result<Self> {
        let target_config = config.target.clone();

        let client = create_kube_api(
            config.accept_invalid_certificates,
            config.kubeconfig.clone(),
        )
        .await?;

        let target_api: Api<TargetCrd> =
            get_k8s_resource_api(&client, target_config.namespace.as_deref());

        Ok(OperatorApi {
            client,
            target_api,
            target_config,
        })
    }

    async fn fetch_target(&self) -> Result<Option<TargetCrd>> {
        let target = self
            .target_config
            .path
            .as_ref()
            .map(TargetCrd::target_name)
            .ok_or(OperatorApiError::InvalidTarget)?;

        match self.target_api.get(&target).await {
            Ok(target) => Ok(Some(target)),
            Err(kube::Error::Api(ErrorResponse { code: 404, .. })) => Ok(None),
            Err(err) => Err(OperatorApiError::from(KubeApiError::from(err))),
        }
    }

    async fn connect_target(
        &self,
        target: TargetCrd,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        let connection = self
            .client
            .connect(
                Request::builder()
                    .uri(format!(
                        "{}/{}?connect=true",
                        self.target_api.resource_url(),
                        target.name()
                    ))
                    .body(vec![])?,
            )
            .await
            .map_err(KubeApiError::from)?;

        wrap_connection(connection)
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
                                eprintln!("{err:?}");

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
                            if let Err(err) = daemon_tx.send(daemon_message).await {
                                eprintln!("{err:?}");

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
