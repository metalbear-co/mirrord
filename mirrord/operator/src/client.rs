use base64::{engine::general_purpose, Engine as _};
use futures::{SinkExt, StreamExt};
use http::request::Request;
use kube::{error::ErrorResponse, Api, Client, Resource};
use mirrord_auth::{
    certificate::Certificate, credential_store::CredentialStoreSync, error::AuthenticationError,
};
use mirrord_config::{
    feature::network::incoming::ConcurrentSteal, target::TargetConfig, LayerConfig,
};
use mirrord_kube::{
    api::{get_k8s_resource_api, kubernetes::create_kube_api},
    error::KubeApiError,
};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};
use tracing::{debug, error};

use crate::crd::{MirrordOperatorCrd, OperatorFeatures, TargetCrd, OPERATOR_STATUS_NAME};

static CONNECTION_CHANNEL_SIZE: usize = 1000;

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

/// Data we store into environment variables for the child processes to use.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OperatorSessionInformation {
    pub client_certificate: Option<Certificate>,
    pub session_id: u64,
    pub target: TargetCrd,
    pub fingerprint: Option<String>,
    pub operator_features: Vec<OperatorFeatures>,
    pub protocol_version: Option<semver::Version>,
}

impl OperatorSessionInformation {
    pub fn new(
        client_certificate: Option<Certificate>,
        target: TargetCrd,
        fingerprint: Option<String>,
        operator_features: Vec<OperatorFeatures>,
        protocol_version: Option<semver::Version>,
    ) -> Self {
        Self {
            client_certificate,
            session_id: rand::random(),
            target,
            fingerprint,
            operator_features,
            protocol_version,
        }
    }
}

pub struct OperatorApi {
    client: Client,
    target_api: Api<TargetCrd>,
    target_namespace: Option<String>,
    version_api: Api<MirrordOperatorCrd>,
    target_config: TargetConfig,
    on_concurrent_steal: ConcurrentSteal,
}

impl OperatorApi {
    /// Creates a new operator session, setting the session information in environment variables.
    pub async fn create_session<P>(
        config: &LayerConfig,
        progress: &P,
    ) -> Result<
        Option<(
            mpsc::Sender<ClientMessage>,
            mpsc::Receiver<DaemonMessage>,
            OperatorSessionInformation,
        )>,
    >
    where
        P: Progress + Send + Sync,
    {
        let operator_api = OperatorApi::new(config).await?;

        if let Some(target) = operator_api.fetch_target().await? {
            let status = operator_api.get_status().await?;

            let mut version_progress = progress.subtask("comparing versions");
            let operator_version = Version::parse(&status.spec.operator_version).unwrap(); // TODO: Remove unwrap

            // This is printed multiple times when the local process forks. Can be solved by e.g.
            // propagating an env var, don't think it's worth the extra complexity though
            let mirrord_version = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
            if operator_version > mirrord_version {
                // we make two sub tasks since it looks best this way
                version_progress.warning(
                    &format!(
                        "Your mirrord plugin/CLI version {} does not match the operator version {}. This can lead to unforeseen issues.",
                        mirrord_version,
                        operator_version));
                version_progress.success(None);
                version_progress = progress.subtask("comparing versions");
                version_progress.warning(
                    "Consider updating your mirrord plugin/CLI to match the operator version.",
                );
            }
            version_progress.success(None);

            let client_certificate =
                if let Some(credential_name) = status.spec.license.fingerprint.as_ref() {
                    CredentialStoreSync::get_client_certificate::<MirrordOperatorCrd>(
                        &operator_api.client,
                        credential_name.to_string(),
                    )
                    .await
                    .map_err(|err| debug!("CredentialStore error: {err}"))
                    .ok()
                } else {
                    None
                };

            let operator_session_information = OperatorSessionInformation::new(
                client_certificate,
                target,
                status.spec.license.fingerprint,
                status.spec.features.unwrap_or_default(),
                status
                    .spec
                    .protocol_version
                    .and_then(|str_version| str_version.parse().ok()),
            );

            let (sender, receiver) = operator_api
                .connect_target(&operator_session_information)
                .await?;
            Ok(Some((sender, receiver, operator_session_information)))
        } else {
            // No operator found
            Ok(None)
        }
    }

    /// Connect to session using operator and session information
    pub async fn connect(
        config: &LayerConfig,
        session_information: &OperatorSessionInformation,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        OperatorApi::new(config)
            .await?
            .connect_target(session_information)
            .await
    }

    async fn new(config: &LayerConfig) -> Result<Self> {
        let target_config = config.target.clone();
        let on_concurrent_steal = config.feature.network.incoming.on_concurrent_steal.clone();

        let client = create_kube_api(
            config.accept_invalid_certificates,
            config.kubeconfig.clone(),
            config.kube_context.clone(),
        )
        .await?;

        let target_namespace = if target_config.path.is_some() {
            target_config.namespace.clone()
        } else {
            // When targetless, pass agent namespace to operator so that it knows where to create
            // the agent (the operator does not get the agent config).
            config.agent.namespace.clone()
        };

        let target_api: Api<TargetCrd> = get_k8s_resource_api(&client, target_namespace.as_deref());

        let version_api: Api<MirrordOperatorCrd> = Api::all(client.clone());

        Ok(OperatorApi {
            client,
            target_api,
            target_namespace,
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

    #[tracing::instrument(level = "debug", skip(self), ret)]
    fn connect_url(&self, session_information: &OperatorSessionInformation) -> String {
        let OperatorApi {
            on_concurrent_steal,
            ..
        } = self;
        let target = &session_information.target;

        if session_information
            .operator_features
            .contains(&OperatorFeatures::ProxyApi)
        {
            let dt = &();
            let ns = self
                .target_namespace
                .as_deref()
                .unwrap_or_else(|| self.client.default_namespace());
            let api_version = TargetCrd::api_version(dt);
            let plural = TargetCrd::plural(dt);

            format!("/apis/{api_version}/proxy/namespaces/{ns}/{plural}/{}?on_concurrent_steal={on_concurrent_steal}&connect=true", target.name())
        } else {
            format!(
                "{}/{}?on_concurrent_steal={on_concurrent_steal}&connect=true",
                self.target_api.resource_url(),
                target.name(),
            )
        }
    }

    /// Create websocket connection to operator
    #[tracing::instrument(level = "trace", skip(self))]
    async fn connect_target(
        &self,
        session_information: &OperatorSessionInformation,
    ) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
        // why are we checking on client side..?
        if self.on_concurrent_steal == ConcurrentSteal::Abort && let Ok(lock_target) = self
                .target_api
                .get_subresource("port-locks", &session_information.target.name())
                .await && lock_target
                .spec
                .port_locks
                .map(|locks| !locks.is_empty())
                .unwrap_or(false) {
            return Err(OperatorApiError::ConcurrentStealAbort);
        }

        let mut builder = Request::builder()
            .uri(self.connect_url(session_information))
            .header("x-session-id", session_information.session_id.to_string());

        if let Some(certificate) = &session_information.client_certificate {
            match certificate
                .encode_der()
                .map(|certificate_der| general_purpose::STANDARD.encode(certificate_der))
                .map_err(AuthenticationError::Pem)
            {
                Ok(client_credentials) => {
                    builder = builder.header("x-client-der", client_credentials);
                }
                Err(err) => {
                    debug!("CredentialStore error: {err}");
                }
            }
        }

        let connection = self
            .client
            .connect(builder.body(vec![])?)
            .await
            .map_err(KubeApiError::from)?;

        Ok(ConnectionWrapper::wrap(
            connection,
            session_information.protocol_version.clone(),
        ))
    }
}

pub struct ConnectionWrapper<T> {
    connection: T,
    client_rx: Receiver<ClientMessage>,
    daemon_tx: Sender<DaemonMessage>,
    protocol_version: Option<semver::Version>,
}

impl<T> ConnectionWrapper<T>
where
    for<'stream> T: StreamExt<Item = Result<Message, TungsteniteError>>
        + SinkExt<Message, Error = TungsteniteError>
        + Send
        + Unpin
        + 'stream,
{
    fn wrap(
        connection: T,
        protocol_version: Option<semver::Version>,
    ) -> (Sender<ClientMessage>, Receiver<DaemonMessage>) {
        let (client_tx, client_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);
        let (daemon_tx, daemon_rx) = mpsc::channel(CONNECTION_CHANNEL_SIZE);

        let connection_wrapper = ConnectionWrapper {
            protocol_version,
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
                        Some(ClientMessage::SwitchProtocolVersion(version)) => {
                            if let Some(operator_protocol_version) = self.protocol_version.as_ref() {
                                self.handle_client_message(ClientMessage::SwitchProtocolVersion(operator_protocol_version.min(&version).clone())).await?;
                            } else {
                                self.daemon_tx
                                    .send(DaemonMessage::SwitchProtocolVersionResponse(
                                        "1.2.1".parse().expect("Bad static version"),
                                    ))
                                    .await
                                    .map_err(|_| OperatorApiError::DaemonReceiverDropped)?;
                            }
                        }
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
