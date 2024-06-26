use std::fmt::{self, Display};

use base64::{engine::general_purpose, Engine as _};
use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt};
use http::{request::Request, HeaderName, HeaderValue};
use kube::{
    api::{ListParams, PostParams},
    Api, Client, Config, Resource,
};
use mirrord_analytics::{AnalyticsHash, AnalyticsOperatorProperties, Reporter};
use mirrord_auth::{
    certificate::Certificate,
    credential_store::{CredentialStoreSync, UserIdentity},
    credentials::LicenseValidity,
};
use mirrord_config::{feature::network::incoming::ConcurrentSteal, target::Target, LayerConfig};
use mirrord_kube::{
    api::kubernetes::{create_kube_config, get_k8s_resource_api},
    error::KubeApiError,
};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message};
use tracing::{error, info, warn, Level};

use crate::{
    crd::{
        CopyTargetCrd, CopyTargetSpec, MirrordOperatorCrd, MirrordOperatorSpec, OperatorFeatures,
        SessionCrd, TargetCrd, OPERATOR_STATUS_NAME,
    },
    types::{CLIENT_CERT_HEADER_NAME, MIRRORD_CLI_VERSION_HEADER_NAME},
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
    SessionManagement,
    ListingTargets,
}

impl Display for OperatorOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::FindingOperator => "finding operator",
            Self::FindingTarget => "finding target",
            Self::WebsocketConnection => "creating a websocket connection",
            Self::CopyingTarget => "copying target",
            Self::GettingStatus => "getting status",
            Self::SessionManagement => "session management",
            Self::ListingTargets => "listing targets",
        };

        f.write_str(as_str)
    }
}

#[derive(Debug, Error)]
pub enum OperatorApiError {
    #[error("failed to build a websocket connect request: {0}")]
    ConnectRequestBuildError(HttpError),

    #[error("failed to create Kubernetes client: {0}")]
    CreateKubeClient(KubeApiError),

    #[error("{operation} failed: {error}")]
    KubeError {
        error: kube::Error,
        operation: OperatorOperation,
    },

    #[error("mirrord operator {operator_version} does not support feature {feature}")]
    UnsupportedFeature {
        feature: String,
        operator_version: String,
    },

    #[error("{operation} failed with code {}: {}", status.code, status.reason)]
    StatusFailure {
        operation: OperatorOperation,
        status: Box<kube::core::Status>,
    },

    #[error("mirrord operator license expired")]
    NoLicense,

    #[error("failed to prepare user certificate: {0}")]
    UserCertError(String),
}

type Result<T, E = OperatorApiError> = std::result::Result<T, E>;

/// Metadata of an operator session.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OperatorSessionMetadata {
    /// Generated in the operator or restored from local
    /// [`CredentialStore`](mirrord_auth::credential_store::CredentialStore).
    client_certificate: Certificate,
    /// Random identifier of this session. Generated locally.
    session_id: u64,
    /// Operator specification fetched from the cluster.
    operator_spec: MirrordOperatorSpec,
}

impl OperatorSessionMetadata {
    fn new(client_certificate: Certificate, operator_spec: MirrordOperatorSpec) -> Self {
        Self {
            client_certificate,
            session_id: rand::random(),
            operator_spec,
        }
    }

    pub fn operator_spec(&self) -> &MirrordOperatorSpec {
        &self.operator_spec
    }

    #[tracing::instrument(level = Level::TRACE, skip(base_config), err)]
    fn get_certified_client(&self, mut base_config: Config) -> Result<Client> {
        let as_der = self.client_certificate.encode_der().map_err(|error| {
            OperatorApiError::UserCertError(format!("failed to encode user certificate: {error}"))
        })?;
        let as_base64 = general_purpose::STANDARD.encode(as_der);
        let as_header = HeaderValue::try_from(as_base64)
            .map_err(|error| OperatorApiError::UserCertError(error.to_string()))?;

        base_config
            .headers
            .push((HeaderName::from_static(CLIENT_CERT_HEADER_NAME), as_header));

        Client::try_from(base_config)
            .map_err(KubeApiError::from)
            .map_err(OperatorApiError::CreateKubeClient)
    }

    fn set_operator_properties<R: Reporter>(&self, analytics: &mut R) {
        let public_key_data = self.client_certificate.public_key_data();
        let client_hash = AnalyticsHash::from_bytes(&public_key_data);

        analytics.set_operator_properties(AnalyticsOperatorProperties {
            client_hash: Some(client_hash),
            license_hash: self
                .operator_spec
                .license
                .fingerprint
                .as_deref()
                .map(AnalyticsHash::from_base64),
        });
    }

    fn proxy_feature_enabled(&self) -> bool {
        let Some(features) = self.operator_spec.features.as_ref() else {
            return false;
        };

        features.contains(&OperatorFeatures::ProxyApi)
    }
}

/// Prepared target of an operator session.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum OperatorSessionTarget {
    /// CRD of an immediate target validated and fetched by the operator.
    Raw(TargetCrd),
    /// CRD of a copied target created by the operator.
    Copied(CopyTargetCrd),
}

/// Information about created operator session and its prepared target.
/// Can be used to restore [`OperatorApi`] with [`OperatorApi::with_exisiting_session`]
/// and connect to the target with [`OperatorApi::connect_target`].
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OperatorSessionInformation {
    pub target: OperatorSessionTarget,
    pub metadata: OperatorSessionMetadata,
}

/// Wrapper over mirrord operator API.
pub struct OperatorApi {
    /// For making requests to kubernetes API server.
    /// This client will send [`MIRRORD_CLI_VERSION_HEADER_NAME`] and [`CLIENT_CERT_HEADER_NAME`]
    /// headers with each request.
    client: Client,
    /// Metadata of the operator session used by this API.
    session_metadata: OperatorSessionMetadata,
}

/// Connection to existing operator session.
pub struct OperatorSessionConnection {
    /// For sending messages to the operator.
    pub tx: Sender<ClientMessage>,
    /// For receiving messages from the operator.
    pub rx: Receiver<DaemonMessage>,
}

impl OperatorApi {
    /// We allow copied pods to live only for 30 seconds before the internal proxy connects.
    const COPIED_POD_IDLE_TTL: u32 = 30;

    /// Checks the given [`LayerConfig`] against operator specification fetched from the cluster.
    fn check_config(layer_config: &LayerConfig, operator: &MirrordOperatorSpec) -> Result<()> {
        let client_wants_copy = layer_config.feature.copy_target.enabled;
        let operator_supports_copy = operator.copy_target_enabled.unwrap_or(false);
        if client_wants_copy && !operator_supports_copy {
            return Err(OperatorApiError::UnsupportedFeature {
                feature: "copy target".into(),
                operator_version: operator.operator_version.clone(),
            });
        }

        Ok(())
    }

    /// Allows us to access the operator's [`SessionCrd`] [`Api`].
    pub fn session_api(&self) -> Api<SessionCrd> {
        Api::all(self.client.clone())
    }

    /// Retrieves client [`Certificate`] from local credential store or creates one using the given
    /// [`Client`] through the operator API.
    #[tracing::instrument(level = "trace", skip(client))]
    async fn get_client_certificate(
        client: &Client,
        operator: &MirrordOperatorCrd,
    ) -> Result<Certificate, OperatorApiError> {
        let Some(fingerprint) = operator.spec.license.fingerprint.clone() else {
            return Err(OperatorApiError::UserCertError(
                "license fingerprint is missing from the mirrord operator resource".to_string(),
            ));
        };

        let subscription_id = operator.spec.license.subscription_id.clone();

        let mut credential_store = CredentialStoreSync::open().await.map_err(|error| {
            OperatorApiError::UserCertError(format!(
                "failed to access local credential store: {error}"
            ))
        })?;

        credential_store
            .get_client_certificate::<MirrordOperatorCrd>(client, fingerprint, subscription_id)
            .await
            .map_err(|error| {
                OperatorApiError::UserCertError(format!(
                    "failed to get client cerfificate: {error}"
                ))
            })
    }

    /// Creates a new instance of this API.
    /// Performs some operator version/license checks and prepares user certificate.
    pub async fn new<P, R>(
        layer_config: &LayerConfig,
        progress: &P,
        analytics: &mut R,
    ) -> Result<Self>
    where
        P: Progress + Send + Sync,
        R: Reporter,
    {
        let mut client_config = create_kube_config(
            layer_config.accept_invalid_certificates,
            layer_config.kubeconfig.clone(),
            layer_config.kube_context.clone(),
        )
        .await
        .map_err(KubeApiError::from)
        .map_err(OperatorApiError::CreateKubeClient)?;

        client_config.headers.push((
            HeaderName::from_static(MIRRORD_CLI_VERSION_HEADER_NAME),
            HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
        ));

        let uncertified_client = Client::try_from(client_config.clone())
            .map_err(KubeApiError::from)
            .map_err(OperatorApiError::CreateKubeClient)?;

        let operator = Self::fetch_operator(uncertified_client.clone()).await?;

        // Warns the user if their license is close to expiring or fallback to OSS if expired
        let Some(days_until_expiration) =
            DateTime::from_naive_date(operator.spec.license.expire_at).days_until_expiration()
        else {
            let no_license_message = "No valid license found for mirrord for Teams, falling back to OSS usage. Visit https://app.metalbear.co to purchase or renew your license.";
            progress.warning(no_license_message);
            warn!(no_license_message);

            return Err(OperatorApiError::NoLicense);
        };

        let expires_soon =
            days_until_expiration <= <DateTime<Utc> as LicenseValidity>::CLOSE_TO_EXPIRATION_DAYS;
        let is_trial = operator.spec.license.name.contains("(Trial)");

        if is_trial && expires_soon {
            let expiring_soon = (days_until_expiration > 0)
                .then(|| {
                    format!(
                        "soon, in {days_until_expiration} day{}",
                        if days_until_expiration > 1 { "s" } else { "" }
                    )
                })
                .unwrap_or_else(|| "today".to_string());

            let expiring_message = format!("Operator license will expire {expiring_soon}!",);

            progress.warning(&expiring_message);
            warn!(expiring_message);
        } else if is_trial {
            let good_validity_message =
                format!("Operator license is valid for {days_until_expiration} more days.");

            progress.info(&good_validity_message);
            info!(good_validity_message);
        }

        let client_certificate =
            Self::get_client_certificate(&uncertified_client, &operator).await?;

        let session_metadata = OperatorSessionMetadata::new(client_certificate, operator.spec);
        let client = session_metadata.get_certified_client(client_config)?;

        let mut version_progress = progress.subtask("comparing versions");
        match Version::parse(&session_metadata.operator_spec.operator_version) {
            Ok(operator_version) => {
                let mirrord_version = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
                if operator_version > mirrord_version {
                    // we make two sub tasks since it looks best this way
                    version_progress.warning(&format!(
                        "Your mirrord plugin/CLI version {} does not match the operator version {}. This can lead to unforeseen issues.",
                        mirrord_version,
                        operator_version
                    ));
                    version_progress.success(None);
                    version_progress = progress.subtask("comparing versions");
                    version_progress.warning(
                        "Consider updating your mirrord plugin/CLI to match the operator version.",
                    );
                }
                version_progress.success(None);
            }
            Err(error) => {
                tracing::debug!(%error, "failed to parse operator version");
                version_progress.failure(Some("failed to parse operator version"));
            }
        }

        session_metadata.set_operator_properties(analytics);

        Ok(Self {
            client,
            session_metadata,
        })
    }

    /// Creates a new instance of this API.
    /// The instance will use the given operator session.
    /// This function skipts initial operator checks and user certificate preparation, as this steps
    /// must have already been done in [`OperatorApi::new`].
    pub async fn with_exisiting_session<R>(
        layer_config: &LayerConfig,
        session_metadata: OperatorSessionMetadata,
        analytics: &mut R,
    ) -> Result<Self>
    where
        R: Reporter,
    {
        let mut client_config = create_kube_config(
            layer_config.accept_invalid_certificates,
            layer_config.kubeconfig.clone(),
            layer_config.kube_context.clone(),
        )
        .await
        .map_err(KubeApiError::from)
        .map_err(OperatorApiError::CreateKubeClient)?;

        client_config.headers.push((
            HeaderName::from_static(MIRRORD_CLI_VERSION_HEADER_NAME),
            HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
        ));

        let client = session_metadata.get_certified_client(client_config)?;

        session_metadata.set_operator_properties(analytics);

        Ok(Self {
            client,
            session_metadata,
        })
    }

    /// Fetches operator resource from the cluster.
    #[tracing::instrument(level = "trace", skip_all, ret)]
    async fn fetch_operator(client: Client) -> Result<MirrordOperatorCrd> {
        Api::all(client)
            .get(OPERATOR_STATUS_NAME)
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::FindingOperator,
            })
    }

    /// Returns a CRD for the target specified in the given [`LayerConfig`].
    /// If `copy_target` feature is enabled, this required creating [`CopyTargetCrd`] first.
    #[tracing::instrument(level = "trace", skip(self, progress))]
    pub async fn get_target<P>(
        &self,
        config: &LayerConfig,
        progress: &P,
    ) -> Result<OperatorSessionTarget>
    where
        P: Progress,
    {
        Self::check_config(config, &self.session_metadata.operator_spec)?;

        if config.feature.copy_target.enabled {
            // We do not validate the `target` here, it's up to the operator.
            let mut copy_progress = progress.subtask("copying target");
            let copied = self.copy_target(config).await?;
            copy_progress.success(None);

            Ok(OperatorSessionTarget::Copied(copied))
        } else {
            let target_name =
                TargetCrd::target_name(config.target.path.as_ref().unwrap_or(&Target::Targetless));

            let raw_target = Api::namespaced(self.client.clone(), self.namespace(config))
                .get(&target_name)
                .await
                .map_err(|error| OperatorApiError::KubeError {
                    error,
                    operation: OperatorOperation::FindingTarget,
                })?;

            Ok(OperatorSessionTarget::Raw(raw_target))
        }
    }

    /// Returns a namespace of the target based on the given [`LayerConfig`].
    fn namespace<'a>(&'a self, config: &'a LayerConfig) -> &'a str {
        let namespace_opt = if config.target.path.is_some() {
            // Not a targetless run, we use target's namespace.
            config.target.namespace.as_deref()
        } else {
            // A targetless run, we use the namespace where the agent should live.
            config.agent.namespace.as_deref()
        };

        namespace_opt.unwrap_or(self.client.default_namespace())
    }

    /// Returns a connection url for the given [`OperatorSessionTarget`].
    /// This can be used to create a websocket connection with the operator.
    #[tracing::instrument(level = "debug", skip(self), ret)]
    fn connect_url(
        &self,
        target: &OperatorSessionTarget,
        concurrent_steal: ConcurrentSteal,
    ) -> String {
        match (self.session_metadata.proxy_feature_enabled(), target) {
            (true, OperatorSessionTarget::Raw(target)) => {
                let name = target.name();
                let namespace = target
                    .meta()
                    .namespace
                    .as_deref()
                    .expect("missing 'TargetCrd' namespace");
                let api_version = TargetCrd::api_version(&());
                let plural = TargetCrd::plural(&());

                format!("/apis/{api_version}/proxy/namespaces/{namespace}/{plural}/{name}?on_concurrent_steal={concurrent_steal}&connect=true")
            }

            (false, OperatorSessionTarget::Raw(target)) => {
                let name = target.name();
                let namespace = target
                    .meta()
                    .namespace
                    .as_deref()
                    .expect("missing 'TargetCrd' namespace");
                let url_path = TargetCrd::url_path(&(), Some(namespace));

                format!("{url_path}/{name}?on_concurrent_steal={concurrent_steal}&connect=true")
            }
            (true, OperatorSessionTarget::Copied(target)) => {
                let name = target
                    .meta()
                    .name
                    .as_deref()
                    .expect("missing 'CopyTargetCrd' name");
                let namespace = target
                    .meta()
                    .namespace
                    .as_deref()
                    .expect("missing 'CopyTargetCrd' namespace");
                let api_version = CopyTargetCrd::api_version(&());
                let plural = CopyTargetCrd::plural(&());

                format!(
                    "/apis/{api_version}/proxy/namespaces/{namespace}/{plural}/{name}?connect=true"
                )
            }
            (false, OperatorSessionTarget::Copied(target)) => {
                let name = target
                    .meta()
                    .name
                    .as_deref()
                    .expect("missing 'CopyTargetCrd' name");
                let namespace = target
                    .meta()
                    .namespace
                    .as_deref()
                    .expect("missing 'CopyTargetCrd' namespace");
                let url_path = CopyTargetCrd::url_path(&(), Some(&namespace));

                format!("{url_path}/{name}?connect=true")
            }
        }
    }

    /// Creates websocket connection to operator.
    #[tracing::instrument(level = "trace", skip(self))]
    pub async fn connect_target(
        &self,
        target: &OperatorSessionTarget,
        concurrent_steal: ConcurrentSteal,
    ) -> Result<OperatorSessionConnection> {
        let UserIdentity { name, hostname } = UserIdentity::load();

        let request = {
            let mut builder = Request::builder()
                .uri(self.connect_url(&target, concurrent_steal))
                .header("x-session-id", self.session_metadata.session_id.to_string());

            // Replace non-ascii (not supported in headers) chars and trim headers.
            if let Some(name) = name {
                builder = builder.header(
                    "x-client-name",
                    name.replace(|c: char| !c.is_ascii(), "").trim(),
                );
            };

            if let Some(hostname) = hostname {
                builder = builder.header(
                    "x-client-hostname",
                    hostname.replace(|c: char| !c.is_ascii(), "").trim(),
                );
            };

            builder
                .body(vec![])
                .map_err(OperatorApiError::ConnectRequestBuildError)?
        };

        let connection = upgrade::connect_ws(&self.client, request)
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::WebsocketConnection,
            })?;

        let protocol_version = self
            .session_metadata
            .operator_spec
            .protocol_version
            .as_ref()
            .and_then(|version| version.parse().ok());
        let (tx, rx) = ConnectionWrapper::wrap(connection, protocol_version);

        Ok(OperatorSessionConnection { tx, rx })
    }

    /// Creates a new [`CopyTargetCrd`] resource using the operator.
    /// This should create a new dummy pod out of the given [`Target`].
    ///
    /// # Note
    ///
    /// `copy_target` feature is not available for all target types.
    /// Target type compatibility is checked by the operator.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn copy_target(&self, config: &LayerConfig) -> Result<CopyTargetCrd> {
        let target = config.target.path.clone().unwrap_or(Target::Targetless);
        let name = TargetCrd::target_name(&target);
        let scale_down = config.feature.copy_target.scale_down;

        let requested = CopyTargetCrd::new(
            &name,
            CopyTargetSpec {
                target,
                idle_ttl: Some(Self::COPIED_POD_IDLE_TTL),
                scale_down,
            },
        );

        Api::namespaced(self.client.clone(), self.namespace(config))
            .create(&PostParams::default(), &requested)
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::CopyingTarget,
            })
    }

    /// List targets in the given namespcae using the operator.
    #[tracing::instrument(level = "trace", skip(self), ret)]
    pub async fn list_targets(&self, namespace: Option<&str>) -> Result<Vec<TargetCrd>> {
        get_k8s_resource_api(&self.client, namespace)
            .list(&ListParams::default())
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::ListingTargets,
            })
            .map(|list| list.items)
    }

    /// Return metadata of this API's operator session.
    pub fn session_metadata(&self) -> &OperatorSessionMetadata {
        &self.session_metadata
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

mod upgrade {
    //! Code copied from [`kube::client`] and adjusted.
    //!
    //! Just like original [`Client::connect`] function, [`connect_ws`] creates a
    //! WebSockets connection. However, original function swallows
    //! [`ErrorResponse`] sent by the operator and returns flat
    //! [`UpgradeConnectionError`]. [`connect_ws`] attempts to
    //! recover the [`ErrorResponse`] - if operator response code is not
    //! [`StatusCode::SWITCHING_PROTOCOLS`], it tries to read
    //! response body and deserialize it.

    use base64::Engine;
    use http::{HeaderValue, Request, Response, StatusCode};
    use http_body_util::BodyExt;
    use hyper_util::rt::TokioIo;
    use kube::{
        client::{Body, UpgradeConnectionError},
        core::ErrorResponse,
        Client, Error, Result,
    };
    use tokio_tungstenite::{tungstenite::protocol::Role, WebSocketStream};

    const WS_PROTOCOL: &str = "v4.channel.k8s.io";

    // Verify upgrade response according to RFC6455.
    // Based on `tungstenite` and added subprotocol verification.
    async fn verify_response(res: Response<Body>, key: &HeaderValue) -> Result<Response<Body>> {
        let status = res.status();

        if status != StatusCode::SWITCHING_PROTOCOLS {
            if status.is_client_error() || status.is_server_error() {
                let error_response = res
                    .into_body()
                    .collect()
                    .await
                    .ok()
                    .map(|body| body.to_bytes())
                    .and_then(|body_bytes| {
                        serde_json::from_slice::<ErrorResponse>(&body_bytes).ok()
                    });

                if let Some(error_response) = error_response {
                    return Err(Error::Api(error_response));
                }
            }

            return Err(Error::UpgradeConnection(
                UpgradeConnectionError::ProtocolSwitch(status),
            ));
        }

        let headers = res.headers();
        if !headers
            .get(http::header::UPGRADE)
            .and_then(|h| h.to_str().ok())
            .map(|h| h.eq_ignore_ascii_case("websocket"))
            .unwrap_or(false)
        {
            return Err(Error::UpgradeConnection(
                UpgradeConnectionError::MissingUpgradeWebSocketHeader,
            ));
        }

        if !headers
            .get(http::header::CONNECTION)
            .and_then(|h| h.to_str().ok())
            .map(|h| h.eq_ignore_ascii_case("Upgrade"))
            .unwrap_or(false)
        {
            return Err(Error::UpgradeConnection(
                UpgradeConnectionError::MissingConnectionUpgradeHeader,
            ));
        }

        let accept_key = tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_ref());
        if !headers
            .get(http::header::SEC_WEBSOCKET_ACCEPT)
            .map(|h| h == &accept_key)
            .unwrap_or(false)
        {
            return Err(Error::UpgradeConnection(
                UpgradeConnectionError::SecWebSocketAcceptKeyMismatch,
            ));
        }

        // Make sure that the server returned the correct subprotocol.
        if !headers
            .get(http::header::SEC_WEBSOCKET_PROTOCOL)
            .map(|h| h == WS_PROTOCOL)
            .unwrap_or(false)
        {
            return Err(Error::UpgradeConnection(
                UpgradeConnectionError::SecWebSocketProtocolMismatch,
            ));
        }

        Ok(res)
    }

    /// Generate a random key for the `Sec-WebSocket-Key` header.
    /// This must be nonce consisting of a randomly selected 16-byte value in base64.
    fn sec_websocket_key() -> HeaderValue {
        let random: [u8; 16] = rand::random();
        base64::engine::general_purpose::STANDARD
            .encode(random)
            .parse()
            .expect("should be valid")
    }

    pub async fn connect_ws(
        client: &Client,
        request: Request<Vec<u8>>,
    ) -> kube::Result<WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>> {
        let (mut parts, body) = request.into_parts();
        parts.headers.insert(
            http::header::CONNECTION,
            HeaderValue::from_static("Upgrade"),
        );
        parts
            .headers
            .insert(http::header::UPGRADE, HeaderValue::from_static("websocket"));
        parts.headers.insert(
            http::header::SEC_WEBSOCKET_VERSION,
            HeaderValue::from_static("13"),
        );
        let key = sec_websocket_key();
        parts
            .headers
            .insert(http::header::SEC_WEBSOCKET_KEY, key.clone());
        // Use the binary subprotocol v4, to get JSON `Status` object in `error` channel (3).
        // There's no official documentation about this protocol, but it's described in
        // [`k8s.io/apiserver/pkg/util/wsstream/conn.go`](https://git.io/JLQED).
        // There's a comment about v4 and `Status` object in
        // [`kublet/cri/streaming/remotecommand/httpstream.go`](https://git.io/JLQEh).
        parts.headers.insert(
            http::header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(WS_PROTOCOL),
        );

        let res = client
            .send(Request::from_parts(parts, Body::from(body)))
            .await?;
        let res = verify_response(res, &key).await?;
        match hyper::upgrade::on(res).await {
            Ok(upgraded) => {
                Ok(
                    WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, None)
                        .await,
                )
            }

            Err(e) => Err(Error::UpgradeConnection(
                UpgradeConnectionError::GetPendingUpgrade(e),
            )),
        }
    }
}
