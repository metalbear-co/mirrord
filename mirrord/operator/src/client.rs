use futures::{SinkExt, StreamExt};
use http::request::Request;
use kube::{error::ErrorResponse, Api, Client};
use mirrord_config::{target::TargetConfig, LayerConfig};
use mirrord_kube::{
    api::{get_k8s_resource_api, kubernetes::create_kube_api},
    error::KubeApiError,
};
use mirrord_progress::{MessageKind, Progress};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use semver::Version;
use thiserror::Error;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};
use tracing::{error, trace, warn};

use crate::crd::{MirrordOperatorCrd, TargetCrd, OPERATOR_STATUS_NAME};

static CONNECTION_CHANNEL_SIZE: usize = 1000;
static MIRRORD_OPERATOR_SESSION_ID: &str = "MIRRORD_OPERATOR_SESSION_ID";

#[derive(Debug, Error)]
pub enum OperatorApiError {
    #[error("unable to create target for TargetConfig")]
    InvalidTarget,
    #[error(transparent)]
    HttpError(#[from] http::Error),
    #[error(transparent)]
    WsError(#[from] TungsteniteError),
    #[error(transparent)]
    KubeApiError(#[from] KubeApiError),
    #[error(transparent)]
    DecodeError(#[from] bincode::error::DecodeError),
    #[error(transparent)]
    EncodeError(#[from] bincode::error::EncodeError),
    #[error("invalid message: {0:?}")]
    InvalidMessage(Message),
    #[error("Receiver<DaemonMessage> was dropped")]
    DaemonReceiverDropped,
}

type Result<T, E = OperatorApiError> = std::result::Result<T, E>;

pub struct OperatorApi {
    client: Client,
    target_api: Api<TargetCrd>,
    version_api: Api<MirrordOperatorCrd>,
    target_config: Option<TargetConfig>,
}

impl OperatorApi {
    pub async fn discover<P>(
        config: &LayerConfig,
        progress: &P,
    ) -> Result<Option<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)>>
    where
        P: Progress + Send + Sync,
    {
        let operator_api = OperatorApi::new(config).await?;

        if let Some(target) = operator_api.fetch_target().await? {
            let operator_version = Version::parse(&operator_api.get_version().await?).unwrap(); // TODO: Remove unwrap

            // This is printed multiple times when the local process forks. Can be solved by e.g.
            // propagating an env var, don't think it's worth the extra complexity though
            let mirrord_version = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
            if operator_version != mirrord_version {
                progress.subtask("comparing versions").print_message(MessageKind::Warning, Some(&format!("Your mirrord version {} does not match the operator version {}. This can lead to unforeseen issues.", mirrord_version, operator_version)));
                if operator_version > mirrord_version {
                    progress.subtask("comparing versions").print_message(
                        MessageKind::Warning,
                        Some(
                            "Consider updating your mirrord version to match the operator version.",
                        ),
                    );
                } else {
                    progress.subtask("comparing versions").print_message(MessageKind::Warning, Some("Consider either updating your operator version to match your mirrord version, or downgrading your mirrord version."));
                }
            }
            operator_api.connect_target(target).await.map(Some)
        } else {
            // No operator found
            Ok(None)
        }
    }

    /// Uses `MIRRORD_OPERATOR_SESSION_ID` to have a persistant session_id across child processes,
    /// if env var is empty or missing random id will be generated and saved in env
    fn session_id() -> u64 {
        std::env::var(MIRRORD_OPERATOR_SESSION_ID)
            .inspect_err(|_| trace!("{MIRRORD_OPERATOR_SESSION_ID} empty, creating new session"))
            .ok()
            .and_then(|val| {
                val.parse()
                    .inspect_err(|err| {
                        warn!("Error parsing {MIRRORD_OPERATOR_SESSION_ID}, creating new session. Err: {err}")
                    })
                    .ok()
            })
            .unwrap_or_else(|| {
                let id = rand::random();
                std::env::set_var(MIRRORD_OPERATOR_SESSION_ID, format!("{id}"));
                id
            })
    }

    async fn new(config: &LayerConfig) -> Result<Self> {
        let target_config = config.target.clone();

        let client = create_kube_api(
            config.accept_invalid_certificates,
            config.kubeconfig.clone(),
        )
        .await?;

        let target_namespace = if let Some(ref target_config) = target_config {
            target_config.namespace.as_deref()
        } else {
            // When targetless, pass agent namespace to operator so that it knows where to create
            // the agent (the operator does not get the agent config).
            config.agent.namespace.as_deref()
        };

        let target_api: Api<TargetCrd> = get_k8s_resource_api(&client, target_namespace);

        let version_api: Api<MirrordOperatorCrd> = Api::all(client.clone());

        Ok(OperatorApi {
            client,
            target_api,
            version_api,
            target_config,
        })
    }

    async fn get_version(&self) -> Result<String> {
        self.version_api
            .get(OPERATOR_STATUS_NAME)
            .await
            .map_err(KubeApiError::KubeError)
            .map_err(OperatorApiError::KubeApiError)
            .map(|status| status.spec.operator_version)
    }

    async fn fetch_target(&self) -> Result<Option<TargetCrd>> {
        let target_name = TargetCrd::target_name_by_optional_config(&self.target_config);

        match self.target_api.get(&target_name).await {
            Ok(target) => Ok(Some(target)),
            Err(kube::Error::Api(ErrorResponse { code: 404, .. })) => Ok(None),
            Err(err) => Err(OperatorApiError::from(KubeApiError::from(err))),
        }
    }

    /// Create websocket connection to operator
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
                    .header("x-session-id", Self::session_id())
                    .body(vec![])?,
            )
            .await
            .map_err(KubeApiError::from)?;

        Ok(ConnectionWrapper::wrap(connection))
    }
}

pub struct ConnectionWrapper<T> {
    connection: T,
    client_rx: Receiver<ClientMessage>,
    daemon_tx: Sender<DaemonMessage>,
}

impl<T> ConnectionWrapper<T>
where
    for<'stream> T: StreamExt<Item = Result<Message, TungsteniteError>>
        + SinkExt<Message, Error = TungsteniteError>
        + Send
        + Unpin
        + 'stream,
{
    fn wrap(connection: T) -> (Sender<ClientMessage>, Receiver<DaemonMessage>) {
        let (client_tx, client_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);
        let (daemon_tx, daemon_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);

        let connection_wrapper = ConnectionWrapper {
            connection,
            client_rx,
            daemon_tx,
        };

        tokio::spawn(async move {
            if let Err(err) = connection_wrapper.start().await {
                error!("{err:?}")
            }
        });

        (client_tx, daemon_rx)
    }

    async fn handle_client_message(&mut self, client_message: ClientMessage) -> Result<()> {
        let payload = bincode::encode_to_vec(client_message, bincode::config::standard())?;

        self.connection.send(payload.into()).await?;

        Ok(())
    }

    async fn handle_daemon_message(
        &mut self,
        daemon_message: Result<Message, TungsteniteError>,
    ) -> Result<()> {
        match daemon_message? {
            Message::Binary(payload) => {
                let (daemon_message, _) = bincode::decode_from_slice::<DaemonMessage, _>(
                    &payload,
                    bincode::config::standard(),
                )?;

                self.daemon_tx
                    .send(daemon_message)
                    .await
                    .map_err(|_| OperatorApiError::DaemonReceiverDropped)
            }
            message => Err(OperatorApiError::InvalidMessage(message)),
        }
    }

    async fn start(mut self) -> Result<()> {
        loop {
            tokio::select! {
                client_message = self.client_rx.recv() => {
                    match client_message {
                        Some(client_message) => self.handle_client_message(client_message).await?,
                        None => break,
                    }
                }
                daemon_message = self.connection.next() => {
                    match daemon_message {
                        Some(daemon_message) => self.handle_daemon_message(daemon_message).await?,
                        None => break,
                    }
                }
            }
        }

        let _ = self.connection.send(Message::Close(None)).await;

        Ok(())
    }
}
