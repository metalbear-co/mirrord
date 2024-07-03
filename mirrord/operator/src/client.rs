use std::fmt;

use base64::{engine::general_purpose, Engine};
use chrono::{DateTime, Utc};
use conn_wrapper::ConnectionWrapper;
use error::{OperatorApiError, OperatorApiResult, OperatorOperation};
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
use mirrord_kube::{api::kubernetes::create_kube_config, error::KubeApiError};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::Level;

use crate::{
    crd::{
        CopyTargetCrd, CopyTargetSpec, MirrordOperatorCrd, OperatorFeatures, TargetCrd,
        OPERATOR_STATUS_NAME,
    },
    types::{
        CLIENT_CERT_HEADER, CLIENT_HOSTNAME_HEADER, CLIENT_NAME_HEADER, MIRRORD_CLI_VERSION_HEADER,
        SESSION_ID_HEADER,
    },
};

mod conn_wrapper;
mod discovery;
pub mod error;
mod upgrade;

/// State of client's [`Certificate`] the should be attached to some operator requests.
pub trait ClientCertificateState: fmt::Debug {}

/// Represents a [`ClientCertificateState`] where we don't have the certificate.
#[derive(Debug)]
pub struct NoClientCert {
    /// [`Config::headers`] here contain some extra entries:
    /// 1. [`CLIENT_HOSTNAME_HEADER`] (if available)
    /// 2. [`CLIENT_NAME_HEADER`] (if available)
    /// 3. [`MIRRORD_CLI_VERSION_HEADER`]
    ///
    /// Can be used to create a certified [`Client`] when the [`Certificate`] is available.
    base_config: Config,
}

impl ClientCertificateState for NoClientCert {}

/// Represents a [`ClientCertificateState`] where have the certificate.
pub struct PreparedClientCert {
    /// Prepared client certificate.
    cert: Certificate,
}

impl fmt::Debug for PreparedClientCert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedClientCert")
            .field("cert_public_key_data", &self.cert.public_key_data())
            .finish()
    }
}

impl ClientCertificateState for PreparedClientCert {}

/// Represents a [`ClientCertificateState`] where we attempted to prepare the certificate and we may
/// have failed.
pub struct MaybeClientCert {
    cert_result: Result<Certificate, OperatorApiError>,
}

impl fmt::Debug for MaybeClientCert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MaybeClientCert")
            .field("cert_result", &self.cert_result)
            .finish()
    }
}

impl ClientCertificateState for MaybeClientCert {}

/// Created operator session. Can be obtained from [`OperatorApi::connect_in_new_session`] and later
/// used in [`OperatorApi::connect_in_existing_session`].
///
/// # Note
///
/// Contains enough information to enable connecting with target without fetching
/// [`MirrordOperatorCrd`] again.
#[derive(Clone, Serialize, Deserialize)]
pub struct OperatorSession {
    /// Random session id, generated locally.
    id: u64,
    /// URL where websocket connection request should be sent.
    connect_url: String,
    /// Client certificate, should be included as header in the websocket connection request.
    client_cert: Certificate,
    /// Operator license fingerprint, right now only for setting [`Reporter`] properties.
    operator_license_fingerprint: Option<String>,
    /// Version of [`mirrord_protocol`] used by the operator.
    /// Used to create [`ConnectionWrapper`].
    operator_protocol_version: Option<Version>,
}

impl fmt::Debug for OperatorSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OperatorSession")
            .field("id", &self.id)
            .field("connect_url", &self.connect_url)
            .field("cert_public_key_data", &self.client_cert.public_key_data())
            .field(
                "operator_license_fingerprint",
                &self.operator_license_fingerprint,
            )
            .field("operator_protocol_version", &self.operator_protocol_version)
            .finish()
    }
}

/// Connection to an operator target.
pub struct OperatorSessionConnection {
    /// Session of this connection.
    pub session: OperatorSession,
    /// Used to send [`ClientMessage`]s to the operator.
    pub tx: Sender<ClientMessage>,
    /// Used to receive [`DaemonMessage`]s from the operator.
    pub rx: Receiver<DaemonMessage>,
}

impl fmt::Debug for OperatorSessionConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tx_queued_messages = self.tx.max_capacity() - self.tx.capacity();
        let rx_queued_messages = self.rx.len();

        f.debug_struct("OperatorSessionConnection")
            .field("session", &self.session)
            .field("tx_closed", &self.tx.is_closed())
            .field("tx_queued_messages", &tx_queued_messages)
            .field("rx_closed", &self.rx.is_closed())
            .field("rx_queued_messages", &rx_queued_messages)
            .finish()
    }
}

/// Prepared target of an operator session.
#[derive(Debug)]
enum OperatorSessionTarget {
    /// CRD of an immediate target validated and fetched from the operator.
    Raw(TargetCrd),
    /// CRD of a copied target created by the operator.
    Copied(CopyTargetCrd),
}

impl OperatorSessionTarget {
    /// Returns a connection url for the given [`OperatorSessionTarget`].
    /// This can be used to create a websocket connection with the operator.
    fn connect_url(&self, use_proxy: bool, concurrent_steal: ConcurrentSteal) -> String {
        match (use_proxy, self) {
            (true, OperatorSessionTarget::Raw(crd)) => {
                let name = TargetCrd::urlfied_name(
                    crd.spec
                        .target
                        .known()
                        .expect("[BUG] Unknown target should never be seen here!"),
                );
                let namespace = crd
                    .meta()
                    .namespace
                    .as_deref()
                    .expect("missing 'TargetCrd' namespace");
                let api_version = TargetCrd::api_version(&());
                let plural = TargetCrd::plural(&());

                format!("/apis/{api_version}/proxy/namespaces/{namespace}/{plural}/{name}?on_concurrent_steal={concurrent_steal}&connect=true")
            }

            (false, OperatorSessionTarget::Raw(crd)) => {
                let name = TargetCrd::urlfied_name(
                    crd.spec
                        .target
                        .known()
                        .expect("[BUG] Unknown target should never be seen here!"),
                );
                let namespace = crd
                    .meta()
                    .namespace
                    .as_deref()
                    .expect("missing 'TargetCrd' namespace");
                let url_path = TargetCrd::url_path(&(), Some(namespace));

                format!("{url_path}/{name}?on_concurrent_steal={concurrent_steal}&connect=true")
            }
            (true, OperatorSessionTarget::Copied(crd)) => {
                let name = crd
                    .meta()
                    .name
                    .as_deref()
                    .expect("missing 'CopyTargetCrd' name");
                let namespace = crd
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
            (false, OperatorSessionTarget::Copied(crd)) => {
                let name = crd
                    .meta()
                    .name
                    .as_deref()
                    .expect("missing 'CopyTargetCrd' name");
                let namespace = crd
                    .meta()
                    .namespace
                    .as_deref()
                    .expect("missing 'CopyTargetCrd' namespace");
                let url_path = CopyTargetCrd::url_path(&(), Some(namespace));

                format!("{url_path}/{name}?connect=true")
            }
        }
    }
}

/// Wrapper over mirrord operator API.
pub struct OperatorApi<C> {
    /// For making requests to kubernetes API server.
    client: Client,
    /// Prepared client certificate. If present, [`Self::client`] sends [`CLIENT_CERT_HEADER`] with
    /// each request.
    client_cert: C,
    /// Fetched operator resource.
    operator: MirrordOperatorCrd,
}

impl<C> fmt::Debug for OperatorApi<C>
where
    C: ClientCertificateState,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OperatorApi")
            .field("default_namespace", &self.client.default_namespace())
            .field("client_cert", &self.client_cert)
            .field("operator_version", &self.operator.spec.operator_version)
            .field(
                "operator_protocol_version",
                &self.operator.spec.protocol_version,
            )
            .field(
                "operator_license_fingerprint",
                &self.operator.spec.license.fingerprint,
            )
            .finish()
    }
}

impl OperatorApi<NoClientCert> {
    /// Fetches [`MirrordOperatorCrd`] from the cluster and creates a new instance of this API.
    #[tracing::instrument(level = Level::TRACE, skip_all, err)]
    pub async fn new<R>(config: &LayerConfig, reporter: &mut R) -> OperatorApiResult<Self>
    where
        R: Reporter,
    {
        let base_config = Self::base_client_config(config).await?;
        let client = Client::try_from(base_config.clone())
            .map_err(KubeApiError::from)
            .map_err(OperatorApiError::CreateKubeClient)?;
        let operator: MirrordOperatorCrd = Api::all(client.clone())
            .get(OPERATOR_STATUS_NAME)
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::FindingOperator,
            })?;

        reporter.set_operator_properties(AnalyticsOperatorProperties {
            client_hash: None,
            license_hash: operator
                .spec
                .license
                .fingerprint
                .as_deref()
                .map(AnalyticsHash::from_base64),
        });

        Ok(Self {
            client,
            client_cert: NoClientCert { base_config },
            operator,
        })
    }

    /// If [`LayerConfig::operator`] is explicitly disabled, returns early with [`None`].
    ///
    /// If [`LayerConfig::operator`] is explicitly enabled, this functions is equivalent to
    /// [`OperatorApi::new`] and never returns [`None`].
    ///
    /// If [`LayerConfig::operator`] is missing, tries to fetch [`MirrordOperatorCrd`] from the
    /// cluster and create a new instance of this API. If fetching the resource fails, an extra
    /// discovery step is made to determine whether the operator is installed. If this extra
    /// step proves that the operator is installed, an error is returned. Otherwise, [`None`] is
    /// returned.
    #[tracing::instrument(level = Level::TRACE, skip_all, err)]
    pub async fn try_new<R>(
        config: &LayerConfig,
        reporter: &mut R,
    ) -> OperatorApiResult<Option<Self>>
    where
        R: Reporter,
    {
        if config.operator == Some(false) {
            return Ok(None);
        }

        let base_config = Self::base_client_config(config).await?;
        let client = Client::try_from(base_config.clone())
            .map_err(KubeApiError::from)
            .map_err(OperatorApiError::CreateKubeClient)?;

        let operator: Result<MirrordOperatorCrd, _> =
            Api::all(client.clone()).get(OPERATOR_STATUS_NAME).await;

        let error = match operator {
            Ok(operator) => {
                reporter.set_operator_properties(AnalyticsOperatorProperties {
                    client_hash: None,
                    license_hash: operator
                        .spec
                        .license
                        .fingerprint
                        .as_deref()
                        .map(AnalyticsHash::from_base64),
                });

                return Ok(Some(Self {
                    client,
                    client_cert: NoClientCert { base_config },
                    operator,
                }));
            }

            Err(error @ kube::Error::Api(..)) if config.operator.is_none() => {
                match discovery::operator_installed(&client).await {
                    Ok(false) | Err(..) => {
                        return Ok(None);
                    }
                    Ok(true) => error,
                }
            }

            Err(error) => error,
        };

        Err(OperatorApiError::KubeError {
            error,
            operation: OperatorOperation::FindingOperator,
        })
    }

    /// Prepares client [`Certificate`] to be sent in all subsequent requests to the operator.
    /// In case of failure, state of this API instance does not change.
    #[tracing::instrument(level = Level::TRACE, skip(reporter))]
    pub async fn prepare_client_cert<R>(self, reporter: &mut R) -> OperatorApi<MaybeClientCert>
    where
        R: Reporter,
    {
        let previous_client = self.client.clone();

        let result = try {
            let certificate = self.get_client_certificate().await?;

            reporter.set_operator_properties(AnalyticsOperatorProperties {
                client_hash: Some(AnalyticsHash::from_bytes(&certificate.public_key_data())),
                license_hash: self
                    .operator
                    .spec
                    .license
                    .fingerprint
                    .as_deref()
                    .map(AnalyticsHash::from_base64),
            });

            let header = Self::make_client_cert_header(&certificate)?;

            let mut config = self.client_cert.base_config;
            config
                .headers
                .push((HeaderName::from_static(CLIENT_CERT_HEADER), header));
            let client = Client::try_from(config)
                .map_err(KubeApiError::from)
                .map_err(OperatorApiError::CreateKubeClient)?;

            (client, certificate)
        };

        match result {
            Ok((new_client, cert)) => OperatorApi {
                client: new_client,
                client_cert: MaybeClientCert {
                    cert_result: Ok(cert),
                },
                operator: self.operator,
            },

            Err(error) => OperatorApi {
                client: previous_client,
                client_cert: MaybeClientCert {
                    cert_result: Err(error),
                },
                operator: self.operator,
            },
        }
    }
}

impl OperatorApi<MaybeClientCert> {
    pub fn inspect_cert_error<F: FnOnce(&OperatorApiError)>(&self, f: F) {
        if let Err(e) = &self.client_cert.cert_result {
            f(e);
        }
    }

    pub fn into_certified(self) -> OperatorApiResult<OperatorApi<PreparedClientCert>> {
        let cert = self.client_cert.cert_result?;

        Ok(OperatorApi {
            client: self.client,
            client_cert: PreparedClientCert { cert },
            operator: self.operator,
        })
    }
}

impl<C> OperatorApi<C>
where
    C: ClientCertificateState,
{
    /// Lists targets in the given namespace.
    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub async fn list_targets(&self, namespace: Option<&str>) -> OperatorApiResult<Vec<TargetCrd>> {
        Api::namespaced(
            self.client.clone(),
            namespace.unwrap_or(self.client.default_namespace()),
        )
        .list(&ListParams::default())
        .await
        .map_err(|error| OperatorApiError::KubeError {
            error,
            operation: OperatorOperation::ListingTargets,
        })
        .map(|list| list.items)
    }

    pub fn check_license_validity<P>(&self, progress: &P) -> OperatorApiResult<()>
    where
        P: Progress,
    {
        let Some(days_until_expiration) =
            DateTime::from_naive_date(self.operator.spec.license.expire_at).days_until_expiration()
        else {
            let no_license_message = "No valid license found for mirrord for Teams. Visit https://app.metalbear.co to purchase or renew your license";
            progress.warning(no_license_message);
            tracing::warn!(no_license_message);

            return Err(OperatorApiError::NoLicense);
        };

        let expires_soon =
            days_until_expiration <= <DateTime<Utc> as LicenseValidity>::CLOSE_TO_EXPIRATION_DAYS;
        let is_trial = self.operator.spec.license.name.contains("(Trial)");

        if is_trial && expires_soon {
            let expiring_soon = if days_until_expiration > 0 {
                format!(
                    "soon, in {days_until_expiration} day{}",
                    if days_until_expiration > 1 { "s" } else { "" }
                )
            } else {
                "today".to_string()
            };
            let message = format!("Operator license will expire {expiring_soon}!",);
            progress.warning(&message);
        } else if is_trial {
            let message =
                format!("Operator license is valid for {days_until_expiration} more days.");
            progress.info(&message);
        }

        Ok(())
    }

    pub fn check_operator_version<P>(&self, progress: &P) -> bool
    where
        P: Progress,
    {
        match Version::parse(&self.operator.spec.operator_version) {
            Ok(operator_version) => {
                let mirrord_version = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();

                if operator_version > mirrord_version {
                    let message = format!(
                        "mirrord binary version {} does not match the operator version {}. Consider updating your mirrord binary.",
                        mirrord_version,
                        operator_version
                    );
                    progress.warning(&message);
                    false
                } else {
                    true
                }
            }

            Err(error) => {
                tracing::debug!(%error, "failed to parse operator version");
                progress.warning("Failed to parse operator version.");
                false
            }
        }
    }

    /// Returns a reference to the operator resource fetched from the cluster.
    pub fn operator(&self) -> &MirrordOperatorCrd {
        &self.operator
    }

    /// Returns a reference to the [`Client`] used by this instance.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Creates a base [`Config`] for creating kube [`Client`]s.
    /// Adds extra headers that we send to the operator with each request:
    /// 1. [`MIRRORD_CLI_VERSION_HEADER`]
    /// 2. [`CLIENT_NAME_HEADER`]
    /// 3. [`CLIENT_HOSTNAME_HEADER`]
    async fn base_client_config(layer_config: &LayerConfig) -> OperatorApiResult<Config> {
        let mut client_config = create_kube_config(
            layer_config.accept_invalid_certificates,
            layer_config.kubeconfig.clone(),
            layer_config.kube_context.clone(),
        )
        .await
        .map_err(KubeApiError::from)
        .map_err(OperatorApiError::CreateKubeClient)?;

        client_config.headers.push((
            HeaderName::from_static(MIRRORD_CLI_VERSION_HEADER),
            HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
        ));

        let UserIdentity { name, hostname } = UserIdentity::load();

        let headers = [
            (CLIENT_NAME_HEADER, name),
            (CLIENT_HOSTNAME_HEADER, hostname),
        ];
        for (name, raw_value) in headers {
            let Some(raw_value) = raw_value else {
                continue;
            };

            // Replace non-ascii (not supported in headers) chars and trim.
            let cleaned = raw_value
                .replace(|c: char| !c.is_ascii(), "")
                .trim()
                .to_string();
            let value = HeaderValue::from_str(&cleaned);
            match value {
                Ok(value) => client_config
                    .headers
                    .push((HeaderName::from_static(name), value)),
                Err(error) => {
                    tracing::debug!(%error, %name, raw_value = raw_value, cleaned, "Invalid header value");
                }
            }
        }

        Ok(client_config)
    }

    /// If `copy_target` feature is enabled in the given [`LayerConfig`], checks that the operator
    /// supports it.
    fn check_copy_target_feature_support(&self, config: &LayerConfig) -> OperatorApiResult<()> {
        let client_wants_copy = config.feature.copy_target.enabled;
        let operator_supports_copy = self.operator.spec.copy_target_enabled.unwrap_or(false);
        if client_wants_copy && !operator_supports_copy {
            return Err(OperatorApiError::UnsupportedFeature {
                feature: "copy target".into(),
                operator_version: self.operator.spec.operator_version.clone(),
            });
        }

        Ok(())
    }

    /// Retrieves client [`Certificate`] from local credential store or requests one from the
    /// operator.
    #[tracing::instrument(level = Level::TRACE, err)]
    async fn get_client_certificate(&self) -> Result<Certificate, OperatorApiError> {
        let Some(fingerprint) = self.operator.spec.license.fingerprint.clone() else {
            return Err(OperatorApiError::ClientCertError(
                "license fingerprint is missing from the mirrord operator resource".to_string(),
            ));
        };

        let subscription_id = self.operator.spec.license.subscription_id.clone();

        let mut credential_store = CredentialStoreSync::open().await.map_err(|error| {
            OperatorApiError::ClientCertError(format!(
                "failed to access local credential store: {error}"
            ))
        })?;

        credential_store
            .get_client_certificate::<MirrordOperatorCrd>(
                &self.client,
                fingerprint,
                subscription_id,
            )
            .await
            .map_err(|error| {
                OperatorApiError::ClientCertError(format!(
                    "failed to get client cerfificate: {error}"
                ))
            })
    }

    /// Transforms the given client [`Certificate`] into a [`HeaderValue`].
    fn make_client_cert_header(certificate: &Certificate) -> Result<HeaderValue, OperatorApiError> {
        let as_der = certificate.encode_der().map_err(|error| {
            OperatorApiError::ClientCertError(format!(
                "failed to encode client certificate: {error}"
            ))
        })?;
        let as_base64 = general_purpose::STANDARD.encode(as_der);
        HeaderValue::try_from(as_base64)
            .map_err(|error| OperatorApiError::ClientCertError(error.to_string()))
    }

    /// Returns a namespace of the target based on the given [`LayerConfig`] and default namespace
    /// of [`Client`] used by this instance.
    fn target_namespace<'a>(&'a self, config: &'a LayerConfig) -> &'a str {
        let namespace_opt = if config.target.path.is_some() {
            // Not a targetless run, we use target's namespace.
            config.target.namespace.as_deref()
        } else {
            // A targetless run, we use the namespace where the agent should live.
            config.agent.namespace.as_deref()
        };

        namespace_opt.unwrap_or(self.client.default_namespace())
    }
}

impl OperatorApi<PreparedClientCert> {
    /// We allow copied pods to live only for 30 seconds before the internal proxy connects.
    const COPIED_POD_IDLE_TTL: u32 = 30;

    /// Starts a new operator session and connects to the target.
    /// Returned [`OperatorSessionConnection::session`] can be later used to create another
    /// connection in the same session with [`OperatorApi::connect_in_existing_session`].
    #[tracing::instrument(
        level = Level::TRACE,
        skip(config, progress),
        fields(
            target_config = ?config.target,
            copy_target_config = ?config.feature.copy_target,
            on_concurrent_steal = ?config.feature.network.incoming.on_concurrent_steal,
        ),
        ret,
        err
    )]
    pub async fn connect_in_new_session<P>(
        &self,
        config: &LayerConfig,
        progress: &P,
    ) -> OperatorApiResult<OperatorSessionConnection>
    where
        P: Progress,
    {
        self.check_copy_target_feature_support(config)?;

        let target = if config.feature.copy_target.enabled {
            let mut copy_subtask = progress.subtask("copying target");

            // We do not validate the `target` here, it's up to the operator.
            let target = config.target.path.clone().unwrap_or(Target::Targetless);
            let scale_down = config.feature.copy_target.scale_down;
            let namespace = self.target_namespace(config);
            let copied = self.copy_target(target, scale_down, namespace).await?;

            copy_subtask.success(Some("target copied"));

            OperatorSessionTarget::Copied(copied)
        } else {
            let mut fetch_subtask = progress.subtask("fetching target");

            let target_name =
                TargetCrd::urlfied_name(config.target.path.as_ref().unwrap_or(&Target::Targetless));
            let raw_target = Api::namespaced(self.client.clone(), self.target_namespace(config))
                .get(&target_name)
                .await
                .map_err(|error| OperatorApiError::KubeError {
                    error,
                    operation: OperatorOperation::FindingTarget,
                })?;

            fetch_subtask.success(Some("target fetched"));

            OperatorSessionTarget::Raw(raw_target)
        };
        let use_proxy_api = self
            .operator
            .spec
            .features
            .as_ref()
            .map(|features| features.contains(&OperatorFeatures::ProxyApi))
            .unwrap_or(false);
        let connect_url = target.connect_url(
            use_proxy_api,
            config.feature.network.incoming.on_concurrent_steal,
        );

        let session = OperatorSession {
            id: rand::random(),
            connect_url,
            client_cert: self.client_cert.cert.clone(),
            operator_license_fingerprint: self.operator.spec.license.fingerprint.clone(),
            operator_protocol_version: self
                .operator
                .spec
                .protocol_version
                .as_ref()
                .and_then(|version| version.parse().ok()),
        };

        let mut connection_subtask = progress.subtask("connecting to the target");
        let (tx, rx) = Self::connect_target(&self.client, &session).await?;
        connection_subtask.success(Some("connected to the target"));

        Ok(OperatorSessionConnection { session, tx, rx })
    }

    /// Creates a new [`CopyTargetCrd`] resource using the operator.
    /// This should create a new dummy pod out of the given [`Target`].
    ///
    /// # Note
    ///
    /// `copy_target` feature is not available for all target types.
    /// Target type compatibility is checked by the operator.
    #[tracing::instrument(level = "trace", err)]
    async fn copy_target(
        &self,
        target: Target,
        scale_down: bool,
        namespace: &str,
    ) -> OperatorApiResult<CopyTargetCrd> {
        let name = TargetCrd::urlfied_name(&target);

        let requested = CopyTargetCrd::new(
            &name,
            CopyTargetSpec {
                target,
                idle_ttl: Some(Self::COPIED_POD_IDLE_TTL),
                scale_down,
            },
        );

        Api::namespaced(self.client.clone(), namespace)
            .create(&PostParams::default(), &requested)
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::CopyingTarget,
            })
    }

    /// Connects to the target, reusing the given [`OperatorSession`].
    #[tracing::instrument(level = Level::TRACE, skip(layer_config, reporter), ret, err)]
    pub async fn connect_in_existing_session<R>(
        layer_config: &LayerConfig,
        session: OperatorSession,
        reporter: &mut R,
    ) -> OperatorApiResult<OperatorSessionConnection>
    where
        R: Reporter,
    {
        reporter.set_operator_properties(AnalyticsOperatorProperties {
            client_hash: Some(AnalyticsHash::from_bytes(
                session.client_cert.public_key_data().as_ref(),
            )),
            license_hash: session
                .operator_license_fingerprint
                .as_ref()
                .map(|fingerprint| AnalyticsHash::from_base64(fingerprint)),
        });

        let mut config = Self::base_client_config(layer_config).await?;
        let cert_header = Self::make_client_cert_header(&session.client_cert)?;
        config
            .headers
            .push((HeaderName::from_static(CLIENT_CERT_HEADER), cert_header));

        let client = Client::try_from(config)
            .map_err(KubeApiError::from)
            .map_err(OperatorApiError::CreateKubeClient)?;

        let (tx, rx) = Self::connect_target(&client, &session).await?;

        Ok(OperatorSessionConnection { tx, rx, session })
    }

    /// Creates websocket connection to the operator target.
    #[tracing::instrument(level = Level::TRACE, skip(client), err)]
    async fn connect_target(
        client: &Client,
        session: &OperatorSession,
    ) -> OperatorApiResult<(Sender<ClientMessage>, Receiver<DaemonMessage>)> {
        let request = Request::builder()
            .uri(&session.connect_url)
            .header(SESSION_ID_HEADER, session.id.to_string())
            .body(vec![])
            .map_err(OperatorApiError::ConnectRequestBuildError)?;

        let connection = upgrade::connect_ws(client, request)
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::WebsocketConnection,
            })?;

        Ok(ConnectionWrapper::wrap(
            connection,
            session.operator_protocol_version.clone(),
        ))
    }
}
