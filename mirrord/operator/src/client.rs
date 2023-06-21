use base64::{engine::general_purpose, Engine as _};
use futures::{SinkExt, StreamExt};
use http::request::Request;
use kube::{error::ErrorResponse, Api, Client};
use mirrord_auth::{credential_store::CredentialStoreSync, error::AuthenticationError};
use mirrord_config::{
    feature::network::incoming::ConcurrentSteal, target::TargetConfig, LayerConfig,
};
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
    #[error(transparent)]
    Authentication(#[from] AuthenticationError),
    #[error("Can't start proccess because other locks exist on target")]
    ConcurrentStealAbort,
}

type Result<T, E = OperatorApiError> = std::result::Result<T, E>;

pub struct OperatorApi {
    client: Client,
    target_api: Api<TargetCrd>,
    version_api: Api<MirrordOperatorCrd>,
    target_config: TargetConfig,
    on_concurrent_steal: ConcurrentSteal,
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
            let status = operator_api.get_status().await?;

            let operator_version = Version::parse(&status.spec.operator_version).unwrap(); // TODO: Remove unwrap

            // This is printed multiple times when the local process forks. Can be solved by e.g.
            // propagating an env var, don't think it's worth the extra complexity though
            let mirrord_version = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
            if operator_version != mirrord_version {
                progress.subtask("comparing versions").print_message(MessageKind::Warning, Some(&format!("Your mirrord plugin/CLI version {} does not match the operator version {}. This can lead to unforeseen issues.", mirrord_version, operator_version)));
                if operator_version > mirrord_version {
                    progress.subtask("comparing versions").print_message(
                        MessageKind::Warning,
                        Some(
                            "Consider updating your mirrord plugin/CLI to match the operator version.",
                        ),
                    );
                } else {
                    progress.subtask("comparing versions").print_message(MessageKind::Warning, Some("Consider either updating your operator version to match your mirrord plugin/CLI version, or downgrading your mirrord plugin/CLI."));
                }
            }

            operator_api
                .connect_target(target, status.spec.license.fingerprint)
                .await
                .map(Some)
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
        let on_concurrent_steal = config.feature.network.incoming.on_concurrent_steal.clone();

        let client = create_kube_api(
            config.accept_invalid_certificates,
            config.kubeconfig.clone(),
        )
        .await?;

        let target_namespace = if target_config.path.is_some() {
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
            on_concurrent_steal,
        })
    }

    async fn get_status(&self) -> Result<MirrordOperatorCrd> {
        self.version_api
            .get(OPERATOR_STATUS_NAME)
            .await
            .map_err(KubeApiError::KubeError)
            .map_err(OperatorApiError::KubeApiError)
    }

    async fn fetch_target(&self) -> Result<Option<TargetCrd>> {
        let target_name = TargetCrd::target_name_by_config(&self.target_config);

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
        credential_name: Option<String>,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        if self.on_concurrent_steal == ConcurrentSteal::Abort && let Ok(lock_target) = self
                .target_api
                .get_subresource("port-locks", &target.name())
                .await && lock_target
                .spec
                .port_locks
                .map(|locks| !locks.is_empty())
                .unwrap_or(false) {
            return Err(OperatorApiError::ConcurrentStealAbort);
        }

        let mut builder = Request::builder()
            .uri(format!(
                "{}/{}?on_concurrent_steal={}&connect=true",
                self.target_api.resource_url(),
                target.name(),
                self.on_concurrent_steal
            ))
            .header("x-session-id", Self::session_id());

        if let Some(credential_name) = credential_name {
            let client_credentials = CredentialStoreSync::get_client_certificate::<
                MirrordOperatorCrd,
            >(&self.client, credential_name)
            .await
            .map(|certificate_der| general_purpose::STANDARD.encode(certificate_der))?;

            builder = builder.header("x-client-der", client_credentials);
        }

        let connection = self
            .client
            .connect(builder.body(vec![])?)
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
