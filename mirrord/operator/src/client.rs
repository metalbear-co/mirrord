use std::{fmt, ops::Not};

use base64::{engine::general_purpose, Engine};
use chrono::{DateTime, Utc};
use conn_wrapper::ConnectionWrapper;
use connect_params::ConnectParams;
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
use mirrord_config::{feature::split_queues::SplitQueuesConfig, target::Target, LayerConfig};
use mirrord_kube::{
    api::{kubernetes::create_kube_config, runtime::RuntimeDataProvider},
    error::KubeApiError,
    resolved::ResolvedTarget,
};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::Level;

use crate::{
    crd::{
        CopyTargetCrd, CopyTargetSpec, MirrordOperatorCrd, NewOperatorFeature, TargetCrd,
        OPERATOR_STATUS_NAME,
    },
    types::{
        CLIENT_CERT_HEADER, CLIENT_HOSTNAME_HEADER, CLIENT_NAME_HEADER, MIRRORD_CLI_VERSION_HEADER,
        SESSION_ID_HEADER,
    },
};

mod conn_wrapper;
mod connect_params;
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
    pub operator_protocol_version: Option<Version>,

    /// Allow the layer to attempt reconnection
    pub allow_reconnect: bool,
}

impl fmt::Debug for OperatorSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OperatorSession")
            .field("id", &format!("{:X}", self.id))
            .field("connect_url", &self.connect_url)
            .field("cert_public_key_data", &self.client_cert.public_key_data())
            .field(
                "operator_license_fingerprint",
                &self.operator_license_fingerprint,
            )
            .field("operator_protocol_version", &self.operator_protocol_version)
            .field("allow_reconnect", &self.allow_reconnect)
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
    /// Attempts to fetch the [`MirrordOperatorCrd`] resource and create an instance of this API.
    /// In case of error response from the Kubernetes API server, executes an extra API discovery
    /// step to confirm that the operator is not installed.
    ///
    /// If certain that the operator is not installed, returns [`None`].
    #[tracing::instrument(level = Level::TRACE, skip_all, err)]
    pub async fn try_new<R>(
        config: &LayerConfig,
        reporter: &mut R,
    ) -> OperatorApiResult<Option<Self>>
    where
        R: Reporter,
    {
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

            Err(error @ kube::Error::Api(..)) => {
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
    pub fn check_license_validity<P>(&self, progress: &P) -> OperatorApiResult<()>
    where
        P: Progress,
    {
        let Some(days_until_expiration) =
            self.operator.spec.license.expire_at.days_until_expiration()
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
        let mirrord_version = Version::parse(env!("CARGO_PKG_VERSION"))
            .expect("Something went wrong when parsing mirrord version!");

        if self.operator.spec.operator_version > mirrord_version {
            let message = format!(
                "mirrord binary version {} does not match the operator version {}. Consider updating your mirrord binary.",
                mirrord_version,
                self.operator.spec.operator_version
            );
            progress.warning(&message);
            false
        } else {
            true
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

    /// Check the operator supports all the operator features required by the user's configuration.
    fn check_feature_support(&self, layer_config: &LayerConfig) -> OperatorApiResult<()> {
        if layer_config.feature.copy_target.enabled {
            self.operator
                .spec
                .require_feature(NewOperatorFeature::CopyTarget)?
        }

        if layer_config.feature.split_queues.sqs().next().is_some() {
            self.operator
                .spec
                .require_feature(NewOperatorFeature::SqsQueueSplitting)?;
        }

        if layer_config.feature.split_queues.kafka().next().is_some() {
            self.operator
                .spec
                .require_feature(NewOperatorFeature::KafkaQueueSplitting)?;
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
}

impl OperatorApi<PreparedClientCert> {
    /// We allow copied pods to live only for 30 seconds before the internal proxy connects.
    const COPIED_POD_IDLE_TTL: u32 = 30;

    /// Starts a new operator session and connects to the target.
    /// Returned [`OperatorSessionConnection::session`] can be later used to create another
    /// connection in the same session with [`OperatorApi::connect_in_existing_session`].
    ///
    /// 2 connections are made to the operator (this means that we hit the target's
    /// `connect_resource` twice):
    ///
    /// 1. The 1st one is here;
    /// 2. The 2nd is on the `AgentConnection::new`;
    #[tracing::instrument(
        level = Level::TRACE,
        skip(layer_config, progress),
        fields(
            target_config = ?layer_config.target,
            copy_target_config = ?layer_config.feature.copy_target,
            on_concurrent_steal = ?layer_config.feature.network.incoming.on_concurrent_steal,
        ),
        ret,
        err
    )]
    pub async fn connect_in_new_session<P>(
        &self,
        target: ResolvedTarget<false>,
        layer_config: &mut LayerConfig,
        progress: &P,
    ) -> OperatorApiResult<OperatorSessionConnection>
    where
        P: Progress,
    {
        self.check_feature_support(layer_config)?;

        let use_proxy_api = self
            .operator
            .spec
            .supported_features()
            .contains(&NewOperatorFeature::ProxyApi);

        let is_empty_deployment = target.empty_deployment();
        let do_copy_target = layer_config.feature.copy_target.enabled
            || is_empty_deployment
            || layer_config.feature.split_queues.sqs().next().is_some()
            || (layer_config.feature.split_queues.kafka().next().is_some()
                && self
                    .operator()
                    .spec
                    .require_feature(NewOperatorFeature::KafkaQueueSplittingDirect)
                    .is_err());
        let (connect_url, session_id) = if do_copy_target {
            let mut copy_subtask = progress.subtask("copying target");

            if layer_config.feature.copy_target.enabled.not() {
                if is_empty_deployment.not() {
                    copy_subtask.info("Creating a copy-target for queue-splitting (even though copy_target was not explicitly set).")
                } else {
                    copy_subtask.info("Creating a copy-target for deployment (even thought copy_target was not explicitly set).")
                }
            }

            // We do not validate the `target` here, it's up to the operator.
            let target = layer_config
                .target
                .path
                .clone()
                .unwrap_or(Target::Targetless);
            let scale_down = layer_config.feature.copy_target.scale_down;
            let namespace = layer_config
                .target
                .namespace
                .as_deref()
                .unwrap_or(self.client.default_namespace());
            let copied = self
                .copy_target(
                    target,
                    scale_down,
                    namespace,
                    layer_config
                        .feature
                        .split_queues
                        .is_set()
                        .then(|| layer_config.feature.split_queues.clone()),
                )
                .await?;

            copy_subtask.success(Some("target copied"));

            (
                Self::copy_target_connect_url(
                    &copied,
                    use_proxy_api,
                    layer_config.profile.as_deref(),
                ),
                copied
                    .status
                    .and_then(|copy_crd| copy_crd.creator_session.id),
            )
        } else {
            let target = target.assert_valid_mirrord_target(self.client()).await?;

            // `targetless` has no `RuntimeData`!
            if matches!(target, ResolvedTarget::Targetless(_)).not() {
                // Extracting runtime data asserts that the user can see at least one pod from the
                // workload/service targets.
                let runtime_data = target
                    .runtime_data(self.client(), target.namespace())
                    .await?;

                if runtime_data.guessed_container {
                    progress.warning(
                        format!(
                            "Target has multiple containers, mirrord picked \"{}\".\
                     To target a different one, include it in the target path.",
                            runtime_data.container_name
                        )
                        .as_str(),
                    );
                }
                layer_config
                    .feature
                    .network
                    .incoming
                    .add_probe_ports_to_http_filter_ports(&runtime_data.containers_probe_ports);

                let stolen_probes = runtime_data
                    .containers_probe_ports
                    .iter()
                    .copied()
                    .filter(|port| {
                        layer_config
                            .feature
                            .network
                            .incoming
                            .steals_port_without_filter(*port)
                    })
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>();

                if stolen_probes.is_empty().not() {
                    progress.warning(&format!(
                        "Your mirrord config may steal HTTP/gRPC health checks configured on ports [{}], \
                        causing Kubernetes to terminate containers on the targeted pods. \
                        Use an HTTP filter to prevent this.",
                        stolen_probes.join(", "),
                    ));
                }
            }

            let use_proxy = self
                .operator
                .spec
                .supported_features()
                .contains(&NewOperatorFeature::ProxyApi);

            let params = ConnectParams::new(layer_config);

            (Self::target_connect_url(use_proxy, &target, &params), None)
        };

        tracing::debug!("connect_url {connect_url:?}");

        let allow_reconnect = self
            .operator
            .spec
            .supported_features()
            .contains(&NewOperatorFeature::LayerReconnect);

        let session = OperatorSession {
            // Re-use the `session_id` generated from the `CopyTargetCrd`, or random if
            // this is not a copy target session.
            id: session_id
                .map(|id| u64::from_str_radix(&id, 16))
                .transpose()?
                .unwrap_or_else(rand::random),
            connect_url,
            client_cert: self.client_cert.cert.clone(),
            operator_license_fingerprint: self.operator.spec.license.fingerprint.clone(),
            operator_protocol_version: self
                .operator
                .spec
                .protocol_version
                .as_ref()
                .and_then(|version| version.parse().ok()),
            allow_reconnect,
        };

        let mut connection_subtask = progress.subtask("connecting to the target");
        let (tx, rx) = Self::connect_target(&self.client, &session).await?;
        connection_subtask.success(Some("connected to the target"));

        Ok(OperatorSessionConnection { session, tx, rx })
    }

    /// Produces the URL for making a connection request to the operator.
    fn target_connect_url(
        use_proxy: bool,
        target: &ResolvedTarget<true>,
        connect_params: &ConnectParams<'_>,
    ) -> String {
        let name = {
            let mut urlfied_name = target.type_().to_string();
            if let Some(target_name) = target.name() {
                urlfied_name.push('.');
                urlfied_name.push_str(target_name);
            }
            if let Some(target_container) = target.container() {
                urlfied_name.push_str(".container.");
                urlfied_name.push_str(target_container);
            }
            urlfied_name
        };

        let namespace = target.namespace().unwrap_or("default");

        if use_proxy {
            let api_version = TargetCrd::api_version(&());
            let plural = TargetCrd::plural(&());
            format!(
                "/apis/{api_version}/proxy/namespaces/{namespace}/{plural}/{name}?{connect_params}"
            )
        } else {
            let url_path = TargetCrd::url_path(&(), Some(namespace));
            format!("{url_path}/{name}?{connect_params}")
        }
    }

    /// Produces the URL for making a copied target connection request to the operator.
    fn copy_target_connect_url(
        crd: &CopyTargetCrd,
        use_proxy: bool,
        profile: Option<&str>,
    ) -> String {
        let name = crd
            .meta()
            .name
            .as_deref()
            .expect("CopyTargetCrd was fetched from the operator and should have a name");
        let namespace = crd
            .meta()
            .namespace
            .as_deref()
            .expect("CopyTargetCrd was fetched from the operator and should have a namespace");
        let api_version = CopyTargetCrd::api_version(&());
        let plural = CopyTargetCrd::plural(&());
        let url_path = CopyTargetCrd::url_path(&(), Some(namespace));

        let connect_params = ConnectParams {
            connect: true,
            on_concurrent_steal: None,
            profile,
            // Kafka splits are passed in the request body.
            kafka_splits: Default::default(),
        };

        if use_proxy {
            format!(
                "/apis/{api_version}/proxy/namespaces/{namespace}/{plural}/{name}?{connect_params}",
            )
        } else {
            format!("{url_path}/{name}?{connect_params}",)
        }
    }

    /// Returns client cert's public key in a base64 encoded string (no padding same like in
    /// operator logic)
    pub fn get_user_id_str(&self) -> String {
        general_purpose::STANDARD_NO_PAD.encode(self.client_cert.cert.public_key_data())
    }

    /// Creates a new or reuses existing [`CopyTargetCrd`] resource using the operator.
    /// If new this should create a new dummy pod out of the given [`Target`].
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
        split_queues: Option<SplitQueuesConfig>,
    ) -> OperatorApiResult<CopyTargetCrd> {
        let user_id = self.get_user_id_str();

        let copy_target_api: Api<CopyTargetCrd> = Api::namespaced(self.client.clone(), namespace);

        let existing_copy_targets =
            copy_target_api
                .list(&ListParams::default())
                .await
                .map_err(|error| OperatorApiError::KubeError {
                    error,
                    operation: OperatorOperation::CopyingTarget,
                })?;

        let copy_target_name = TargetCrd::urlfied_name(&target);
        let copy_target_spec = CopyTargetSpec {
            target,
            idle_ttl: Some(Self::COPIED_POD_IDLE_TTL),
            scale_down,
            split_queues,
        };

        if let Some(copy_target) = existing_copy_targets.items.into_iter().find(|copy_target| {
            copy_target.spec == copy_target_spec
                && copy_target
                    .status
                    .as_ref()
                    .map(|status| status.creator_session.user_id.as_ref() == Some(&user_id))
                    .unwrap_or(false)
        }) {
            tracing::debug!(?copy_target, "reusing copy_target");

            return Ok(copy_target);
        }

        copy_target_api
            .create(
                &PostParams::default(),
                &CopyTargetCrd::new(&copy_target_name, copy_target_spec),
            )
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

#[cfg(test)]
mod test {
    use std::collections::{BTreeMap, HashMap};

    use k8s_openapi::api::apps::v1::Deployment;
    use kube::api::ObjectMeta;
    use mirrord_config::feature::network::incoming::ConcurrentSteal;
    use mirrord_kube::resolved::{ResolvedResource, ResolvedTarget};
    use rstest::rstest;

    use super::OperatorApi;
    use crate::client::connect_params::ConnectParams;

    /// Verifies that [`OperatorApi::target_connect_url`] produces expected URLs.
    ///
    /// These URLs should not change for backward compatibility.
    #[rstest]
    #[case::deployment_no_container_no_proxy(
        false,
        ResolvedTarget::Deployment(ResolvedResource {
            resource: Deployment {
                metadata: ObjectMeta {
                    name: Some("py-serv-deployment".into()),
                    namespace: Some("default".into()),
                    ..Default::default()
                },
                spec: None,
                status: None,
            },
            container: None,
        }),
        ConcurrentSteal::Abort,
        None,
        Default::default(),
        "/apis/operator.metalbear.co/v1/namespaces/default/targets/deployment.py-serv-deployment?connect=true&on_concurrent_steal=abort"
    )]
    #[case::deployment_no_container_proxy(
        true,
        ResolvedTarget::Deployment(ResolvedResource {
            resource: Deployment {
                metadata: ObjectMeta {
                    name: Some("py-serv-deployment".into()),
                    namespace: Some("default".into()),
                    ..Default::default()
                },
                spec: None,
                status: None,
            },
            container: None,
        }),
        ConcurrentSteal::Abort,
        None,
        Default::default(),
        "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment?connect=true&on_concurrent_steal=abort"
    )]
    #[case::deployment_container_no_proxy(
        false,
        ResolvedTarget::Deployment(ResolvedResource {
            resource: Deployment {
                metadata: ObjectMeta {
                    name: Some("py-serv-deployment".into()),
                    namespace: Some("default".into()),
                    ..Default::default()
                },
                spec: None,
                status: None,
            },
            container: Some("py-serv".into()),
        }),
        ConcurrentSteal::Abort,
        None,
        Default::default(),
        "/apis/operator.metalbear.co/v1/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv?connect=true&on_concurrent_steal=abort"
    )]
    #[case::deployment_container_proxy(
        true,
        ResolvedTarget::Deployment(ResolvedResource {
            resource: Deployment {
                metadata: ObjectMeta {
                    name: Some("py-serv-deployment".into()),
                    namespace: Some("default".into()),
                    ..Default::default()
                },
                spec: None,
                status: None,
            },
            container: Some("py-serv".into()),
        }),
        ConcurrentSteal::Abort,
        None,
        Default::default(),
        "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv?connect=true&on_concurrent_steal=abort"
    )]
    #[case::deployment_container_proxy_profile(
        true,
        ResolvedTarget::Deployment(ResolvedResource {
            resource: Deployment {
                metadata: ObjectMeta {
                    name: Some("py-serv-deployment".into()),
                    namespace: Some("default".into()),
                    ..Default::default()
                },
                spec: None,
                status: None,
            },
            container: Some("py-serv".into()),
        }),
        ConcurrentSteal::Abort,
        Some("no-steal"),
        Default::default(),
        "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv?connect=true&on_concurrent_steal=abort&profile=no-steal"
    )]
    #[case::deployment_container_proxy_profile_escape(
        true,
        ResolvedTarget::Deployment(ResolvedResource {
            resource: Deployment {
                metadata: ObjectMeta {
                    name: Some("py-serv-deployment".into()),
                    namespace: Some("default".into()),
                    ..Default::default()
                },
                spec: None,
                status: None,
            },
            container: Some("py-serv".into()),
        }),
        ConcurrentSteal::Abort,
        Some("/should?be&escaped"),
        Default::default(),
        "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv?connect=true&on_concurrent_steal=abort&profile=%2Fshould%3Fbe%26escaped"
    )]
    #[case::deployment_container_proxy_kafka_splits(
        true,
        ResolvedTarget::Deployment(ResolvedResource {
            resource: Deployment {
                metadata: ObjectMeta {
                    name: Some("py-serv-deployment".into()),
                    namespace: Some("default".into()),
                    ..Default::default()
                },
                spec: None,
                status: None,
            },
            container: Some("py-serv".into()),
        }),
        ConcurrentSteal::Abort,
        None,
        HashMap::from([(
            "topic-id",
            BTreeMap::from([
                ("header-1".to_string(), "filter-1".to_string()),
                ("header-2".to_string(), "filter-2".to_string()),
            ]),
        )]),
        "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv\
        ?connect=true&on_concurrent_steal=abort&kafka_splits=%7B%22topic-id%22%3A%7B%22header-1%22%3A%22filter-1%22%2C%22header-2%22%3A%22filter-2%22%7D%7D",
    )]
    #[test]
    fn target_connect_url(
        #[case] use_proxy: bool,
        #[case] target: ResolvedTarget<true>,
        #[case] concurrent_steal: ConcurrentSteal,
        #[case] profile: Option<&str>,
        #[case] kafka_splits: HashMap<&str, BTreeMap<String, String>>,
        #[case] expected: &str,
    ) {
        let kafka_splits = kafka_splits
            .iter()
            .map(|(topic_id, filters)| (*topic_id, filters))
            .collect();

        let params = ConnectParams {
            connect: true,
            on_concurrent_steal: Some(concurrent_steal),
            profile,
            kafka_splits,
        };

        let produced = OperatorApi::target_connect_url(use_proxy, &target, &params);
        assert_eq!(produced, expected)
    }
}
