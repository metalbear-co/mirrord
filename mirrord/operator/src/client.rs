use std::{
    fmt::{self, Display},
    io,
};

use base64::{engine::general_purpose, Engine as _};
use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt};
use http::request::Request;
use kube::{api::PostParams, Api, Client, Resource};
use mirrord_analytics::{AnalyticsHash, AnalyticsOperatorProperties, AnalyticsReporter};
use mirrord_auth::{
    certificate::Certificate,
    credential_store::{CredentialStoreSync, UserIdentity},
    credentials::LicenseValidity,
    error::AuthenticationError,
};
use mirrord_config::{
    feature::network::incoming::ConcurrentSteal,
    target::{Target, TargetConfig},
    LayerConfig,
};
use mirrord_kube::{
    api::kubernetes::{create_kube_api, get_k8s_resource_api},
    error::KubeApiError,
};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};
use tracing::{debug, error, warn};

use crate::crd::{
    CopyTargetCrd, CopyTargetSpec, MirrordOperatorCrd, OperatorFeatures, SessionsCrd, TargetCrd,
    OPERATOR_STATUS_NAME,
};

static CONNECTION_CHANNEL_SIZE: usize = 1000;

pub use http::Error as HttpError;

/// Operations performed on the operator via [`kube`] API.
#[derive(Debug)]
pub enum OperatorOperation {
    FindingOperator,
    FindingTarget,
    WebsocketConnection,
    CopyingTarget,
    GettingStatus,
}

impl Display for OperatorOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::FindingOperator => "finding operator",
            Self::FindingTarget => "finding target",
            Self::WebsocketConnection => "creating a websocket connection",
            Self::CopyingTarget => "copying target",
            Self::GettingStatus => "getting status",
        };

        f.write_str(as_str)
    }
}

#[derive(Debug, Error)]
pub enum OperatorApiError {
    #[error("invalid target: {reason}")]
    InvalidTarget { reason: String },
    #[error("failed to build a websocket connect request: {0}")]
    ConnectRequestBuildError(HttpError),
    #[error("failed to create mirrord operator API: {0}")]
    CreateApiError(KubeApiError),
    #[error("{operation} failed: {error}")]
    KubeError {
        error: kube::Error,
        operation: OperatorOperation,
    },
    #[error("can't start proccess because other locks exist on target")]
    ConcurrentStealAbort,
    #[error("mirrord operator {operator_version} does not support feature {feature}")]
    UnsupportedFeature {
        feature: String,
        operator_version: String,
    },
}

type Result<T, E = OperatorApiError> = std::result::Result<T, E>;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OperatorSessionMetadata {
    client_certificate: Option<Certificate>,
    session_id: u64,
    fingerprint: Option<String>,
    operator_features: Vec<OperatorFeatures>,
    protocol_version: Option<semver::Version>,
    copy_pod_enabled: Option<bool>,
}

impl OperatorSessionMetadata {
    fn new(
        client_certificate: Option<Certificate>,
        fingerprint: Option<String>,
        operator_features: Vec<OperatorFeatures>,
        protocol_version: Option<semver::Version>,
        copy_pod_enabled: Option<bool>,
    ) -> Self {
        Self {
            client_certificate,
            session_id: rand::random(),
            fingerprint,
            operator_features,
            protocol_version,
            copy_pod_enabled,
        }
    }

    fn client_credentials(&self) -> io::Result<Option<String>> {
        self.client_certificate
            .as_ref()
            .map(|cert| {
                cert.encode_der()
                    .map(|bytes| general_purpose::STANDARD.encode(bytes))
            })
            .transpose()
    }

    fn set_operator_properties(&self, analytics: &mut AnalyticsReporter) {
        analytics.set_operator_properties(AnalyticsOperatorProperties {
            client_hash: self
                .client_certificate
                .as_ref()
                .and_then(|certificate| certificate.sha256_fingerprint().ok())
                .map(|fingerprint| AnalyticsHash::from_bytes(fingerprint.as_ref())),
            license_hash: self.fingerprint.as_deref().map(AnalyticsHash::from_base64),
        });
    }

    fn proxy_feature_enabled(&self) -> bool {
        self.operator_features.contains(&OperatorFeatures::ProxyApi)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum OperatorSessionTarget {
    Raw(TargetCrd),
    Copied(CopyTargetCrd),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OperatorSessionInformation {
    target: OperatorSessionTarget,
    metadata: OperatorSessionMetadata,
}

pub struct OperatorApi {
    client: Client,
    target_api: Api<TargetCrd>,
    copy_target_api: Api<CopyTargetCrd>,
    sessions_api: Api<SessionsCrd>,
    target_namespace: Option<String>,
    target_config: TargetConfig,
    on_concurrent_steal: ConcurrentSteal,
}

/// Connection to existing operator session.
pub struct OperatorSessionConnection {
    /// For sending messages to the operator.
    pub tx: Sender<ClientMessage>,
    /// For receiving messages from the operator.
    pub rx: Receiver<DaemonMessage>,
    /// Additional data about the session.
    pub info: OperatorSessionInformation,
}

pub async fn session_api(config: Option<String>) -> Result<Api<SessionsCrd>> {
    let kube_api: Client = create_kube_api(false, config, None).await.unwrap();

    Ok(Api::all(kube_api))
}

impl OperatorApi {
    /// We allow copied pods to live only for 30 seconds before the internal proxy connects.
    const COPIED_POD_IDLE_TTL: u32 = 30;

    /// Checks used config against operator specification.
    fn check_config(config: &LayerConfig, operator: &MirrordOperatorCrd) -> Result<()> {
        if config.feature.copy_target.enabled {
            let feature_enabled = operator.spec.copy_target_enabled.unwrap_or(false);

            if !feature_enabled {
                return Err(OperatorApiError::UnsupportedFeature {
                    feature: "copy target".into(),
                    operator_version: operator.spec.operator_version.clone(),
                });
            }

            if config.feature.copy_target.scale_down
                && !matches!(config.target.path, Some(Target::Deployment(..)))
            {
                return Err(OperatorApiError::InvalidTarget {
                    reason: "scale down feature is enabled, but target is not a deployment".into(),
                });
            }
        }

        Ok(())
    }

    #[tracing::instrument(level = "debug", skip(api))]
    pub async fn get_client_certificate(
        api: &OperatorApi,
        operator: &MirrordOperatorCrd,
    ) -> Result<Option<Certificate>, AuthenticationError> {
        let Some(fingerprint) = operator.spec.license.fingerprint.clone() else {
            return Ok(None);
        };

        let subscription_id = operator.spec.license.subscription_id.clone();

        let mut credential_store = CredentialStoreSync::open().await?;
        credential_store
            .get_client_certificate::<MirrordOperatorCrd>(&api.client, fingerprint, subscription_id)
            .await
            .map(Some)
    }

    /// Creates new [`OperatorSessionConnection`] based on the given [`LayerConfig`].
    /// Keep in mind that some failures here won't stop mirrord from hooking into the process
    /// and working, it'll just work without the operator.
    ///
    /// For a fuller documentation, see the docs in `operator/service/src/main.rs::listen`.
    #[tracing::instrument(level = "trace", skip_all)]
    pub async fn create_session<P>(
        config: &LayerConfig,
        progress: &P,
        analytics: &mut AnalyticsReporter,
    ) -> Result<OperatorSessionConnection>
    where
        P: Progress + Send + Sync,
    {
        let operator_api = OperatorApi::new(config).await?;

        let operator = operator_api.fetch_operator().await?;

        // Warns the user if their license is close to expiring.
        //
        // I(alex) considered doing a check for validity also here for expired licenses,
        // but maybe the time of the local user and of the operator are out of sync, so we
        // could end up blocking a valid license (or even just warning on it could be
        // confusing).
        if let Some(expiring_soon) =
            DateTime::from_naive_date(operator.spec.license.expire_at).days_until_expiration()
            && (expiring_soon <= <DateTime<Utc> as LicenseValidity>::CLOSE_TO_EXPIRATION_DAYS)
        {
            let expiring_message = format!(
                "Operator license will expire soon, in {} days!",
                expiring_soon,
            );

            progress.warning(&expiring_message);
            warn!(expiring_message);
        }

        Self::check_config(config, &operator)?;

        let client_certificate = Self::get_client_certificate(&operator_api, &operator)
            .await
            .ok()
            .flatten();
        let metadata = OperatorSessionMetadata::new(
            client_certificate,
            operator.spec.license.fingerprint,
            operator.spec.features.unwrap_or_default(),
            operator
                .spec
                .protocol_version
                .and_then(|str_version| str_version.parse().ok()),
            operator.spec.copy_target_enabled,
        );

        metadata.set_operator_properties(analytics);

        let mut version_progress = progress.subtask("comparing versions");
        let operator_version = Version::parse(&operator.spec.operator_version)
            .expect("failed to parse operator version from operator crd"); // TODO: Remove expect

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

        let raw_target = operator_api.fetch_target().await?;

        let target_to_connect = if config.feature.copy_target.enabled {
            let mut copy_progress = progress.subtask("copying target");

            if config.feature.copy_target.scale_down {
                let is_deployment = matches!(config.target.path, Some(Target::Deployment(..)));
                if !is_deployment {
                    progress.warning(
                        "cannot scale down while copying target - target is not a deployment",
                    )
                }
            }

            let copied = operator_api
                .copy_target(&metadata, raw_target, config.feature.copy_target.scale_down)
                .await?;
            copy_progress.success(None);

            OperatorSessionTarget::Copied(copied)
        } else {
            OperatorSessionTarget::Raw(raw_target)
        };

        let session_info = OperatorSessionInformation {
            target: target_to_connect,
            metadata,
        };
        let connection = operator_api.connect_target(session_info).await?;

        Ok(connection)
    }

    /// Connects to exisiting operator session based on the given [`LayerConfig`] and
    /// [`OperatorSessionInformation`].
    pub async fn connect(
        config: &LayerConfig,
        session_information: OperatorSessionInformation,
        analytics: Option<&mut AnalyticsReporter>,
    ) -> Result<OperatorSessionConnection> {
        if let Some(analytics) = analytics {
            session_information
                .metadata
                .set_operator_properties(analytics);
        }

        let operator_api = OperatorApi::new(config).await?;
        operator_api.connect_target(session_information).await
    }

    async fn new(config: &LayerConfig) -> Result<Self> {
        let target_config = config.target.clone();
        let on_concurrent_steal = config.feature.network.incoming.on_concurrent_steal;

        let client = create_kube_api(
            config.accept_invalid_certificates,
            config.kubeconfig.clone(),
            config.kube_context.clone(),
        )
        .await
        .map_err(OperatorApiError::CreateApiError)?;

        let target_namespace = if target_config.path.is_some() {
            target_config.namespace.clone()
        } else {
            // When targetless, pass agent namespace to operator so that it knows where to create
            // the agent (the operator does not get the agent config).
            config.agent.namespace.clone()
        };

        let target_api: Api<TargetCrd> = get_k8s_resource_api(&client, target_namespace.as_deref());
        let copy_target_api: Api<CopyTargetCrd> =
            get_k8s_resource_api(&client, target_namespace.as_deref());

        let sessions_api = session_api(None).await?;

        Ok(OperatorApi {
            client,
            target_api,
            copy_target_api,
            sessions_api,
            target_namespace,
            target_config,
            on_concurrent_steal,
        })
    }

    #[tracing::instrument(level = "trace", skip(self), ret)]
    async fn fetch_operator(&self) -> Result<MirrordOperatorCrd> {
        let api: Api<MirrordOperatorCrd> = Api::all(self.client.clone());
        api.get(OPERATOR_STATUS_NAME)
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::FindingOperator,
            })
    }

    /// See `operator/controller/src/target.rs::TargetProvider::get_resource`.
    #[tracing::instrument(level = "trace", fields(self.target_config), skip(self))]
    async fn fetch_target(&self) -> Result<TargetCrd> {
        let target_name = TargetCrd::target_name_by_config(&self.target_config);
        self.target_api
            .get(&target_name)
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::FindingTarget,
            })
    }

    /// Returns a namespace of the target.
    fn namespace(&self) -> &str {
        self.target_namespace
            .as_deref()
            .unwrap_or_else(|| self.client.default_namespace())
    }

    /// Returns a connection url for the given [`OperatorSessionInformation`].
    /// This can be used to create a websocket connection with the operator.
    #[tracing::instrument(level = "debug", skip(self), ret)]
    fn connect_url(&self, session: &OperatorSessionInformation) -> String {
        match (session.metadata.proxy_feature_enabled(), &session.target) {
            (true, OperatorSessionTarget::Raw(target)) => {
                let dt = &();
                let namespace = self.namespace();
                let api_version = TargetCrd::api_version(dt);
                let plural = TargetCrd::plural(dt);

                format!(
                    "/apis/{api_version}/proxy/namespaces/{namespace}/{plural}/{}?on_concurrent_steal={}&connect=true",
                    target.name(),
                    self.on_concurrent_steal,
                )
            }
            (false, OperatorSessionTarget::Raw(target)) => {
                format!(
                    "{}/{}?on_concurrent_steal={}&connect=true",
                    self.target_api.resource_url(),
                    target.name(),
                    self.on_concurrent_steal,
                )
            }
            (true, OperatorSessionTarget::Copied(target)) => {
                let dt = &();
                let namespace = self.namespace();
                let api_version = CopyTargetCrd::api_version(dt);
                let plural = CopyTargetCrd::plural(dt);

                format!(
                    "/apis/{api_version}/proxy/namespaces/{namespace}/{plural}/{}?connect=true",
                    target
                        .meta()
                        .name
                        .as_ref()
                        .expect("missing 'copytarget' name"),
                )
            }
            (false, OperatorSessionTarget::Copied(target)) => {
                format!(
                    "{}/{}?connect=true",
                    self.copy_target_api.resource_url(),
                    target
                        .meta()
                        .name
                        .as_ref()
                        .expect("missing 'copytarget' name"),
                )
            }
        }
    }

    /// Checks that there are no active port locks on the given target.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn check_no_port_locks(&self, target: &TargetCrd) -> Result<()> {
        let Ok(lock_target) = self
            .target_api
            .get_subresource("port-locks", &target.name())
            .await
        else {
            return Ok(());
        };

        let no_port_locks = lock_target
            .spec
            .port_locks
            .as_ref()
            .map(Vec::is_empty)
            .unwrap_or(true);

        if no_port_locks {
            Ok(())
        } else {
            Err(OperatorApiError::ConcurrentStealAbort)
        }
    }

    /// Create websocket connection to operator.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn connect_target(
        &self,
        session_info: OperatorSessionInformation,
    ) -> Result<OperatorSessionConnection> {
        // why are we checking on client side..?
        if let (ConcurrentSteal::Abort, OperatorSessionTarget::Raw(target)) =
            (self.on_concurrent_steal, &session_info.target)
        {
            self.check_no_port_locks(target).await?;
        }

        let UserIdentity { name, hostname } = UserIdentity::load();

        let request = {
            let mut builder = Request::builder()
                .uri(self.connect_url(&session_info))
                .header("x-session-id", session_info.metadata.session_id.to_string());

            if let Some(name) = name {
                builder = builder.header("x-client-name", name);
            };

            if let Some(hostname) = hostname {
                builder = builder.header("x-client-hostname", hostname);
            };

            match session_info.metadata.client_credentials() {
                Ok(Some(credentials)) => {
                    builder = builder.header("x-client-der", credentials);
                }
                Ok(None) => {}
                Err(err) => {
                    debug!("CredentialStore error: {err}");
                }
            }

            builder
                .body(vec![])
                .map_err(OperatorApiError::ConnectRequestBuildError)?
        };

        let connection =
            self.client
                .connect(request)
                .await
                .map_err(|error| OperatorApiError::KubeError {
                    error,
                    operation: OperatorOperation::WebsocketConnection,
                })?;

        let (tx, rx) =
            ConnectionWrapper::wrap(connection, session_info.metadata.protocol_version.clone());

        Ok(OperatorSessionConnection {
            tx,
            rx,
            info: session_info,
        })
    }

    /// Creates a new [`CopyTargetCrd`] resource using the operator.
    /// This should create a new dummy pod out of the given `target`.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn copy_target(
        &self,
        session_metadata: &OperatorSessionMetadata,
        target: TargetCrd,
        scale_down: bool,
    ) -> Result<CopyTargetCrd> {
        let raw_target = target
            .spec
            .target
            .clone()
            .ok_or(OperatorApiError::InvalidTarget {
                reason: "copy target feature is not compatible with targetless mode".into(),
            })?;

        let requested = CopyTargetCrd::new(
            &target.name(),
            CopyTargetSpec {
                target: raw_target,
                idle_ttl: Some(Self::COPIED_POD_IDLE_TTL),
                scale_down,
            },
        );

        self.copy_target_api
            .create(&PostParams::default(), &requested)
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::CopyingTarget,
            })
    }
}

#[derive(Error, Debug)]
enum ConnectionWrapperError {
    #[error(transparent)]
    DecodeError(#[from] bincode::error::DecodeError),
    #[error(transparent)]
    EncodeError(#[from] bincode::error::EncodeError),
    #[error(transparent)]
    WsError(#[from] TungsteniteError),
    #[error("invalid message: {0:?}")]
    InvalidMessage(Message),
    #[error("message channel is closed")]
    ChannelClosed,
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

    async fn handle_client_message(
        &mut self,
        client_message: ClientMessage,
    ) -> Result<(), ConnectionWrapperError> {
        let payload = bincode::encode_to_vec(client_message, bincode::config::standard())?;

        self.connection.send(payload.into()).await?;

        Ok(())
    }

    async fn handle_daemon_message(
        &mut self,
        daemon_message: Result<Message, TungsteniteError>,
    ) -> Result<(), ConnectionWrapperError> {
        match daemon_message? {
            Message::Binary(payload) => {
                let (daemon_message, _) = bincode::decode_from_slice::<DaemonMessage, _>(
                    &payload,
                    bincode::config::standard(),
                )?;

                self.daemon_tx
                    .send(daemon_message)
                    .await
                    .map_err(|_| ConnectionWrapperError::ChannelClosed)
            }
            message => Err(ConnectionWrapperError::InvalidMessage(message)),
        }
    }

    async fn start(mut self) -> Result<(), ConnectionWrapperError> {
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
                                    .map_err(|_| ConnectionWrapperError::ChannelClosed)?;
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
