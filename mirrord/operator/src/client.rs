use std::{fmt, ops::Not, time::Duration};

use base64::{Engine, engine::general_purpose};
use chrono::{DateTime, Utc};
use conn_wrapper::ConnectionWrapper;
use connect_params::ConnectParams;
use error::{OperatorApiError, OperatorApiResult, OperatorOperation};
use http::{HeaderName, HeaderValue, request::Request};
use kube::{
    Api, Client, Config, Resource,
    api::{ListParams, PostParams},
    client::ClientBuilder,
};
use mirrord_analytics::{AnalyticsHash, AnalyticsOperatorProperties, Reporter};
use mirrord_auth::{
    certificate::Certificate,
    credential_store::{CredentialStoreSync, UserIdentity},
    credentials::{CiApiKey, Credentials, LicenseValidity},
};
use mirrord_config::{LayerConfig, target::Target};
use mirrord_kube::{
    api::{kubernetes::create_kube_config, runtime::RuntimeDataProvider},
    error::KubeApiError,
    resolved::ResolvedTarget,
    retry::RetryKube,
};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{Receiver, Sender};
use tower::{buffer::BufferLayer, retry::RetryLayer};
use tracing::Level;

use crate::{
    client::database_branches::{
        DatabaseBranchParams, create_mysql_branches, list_reusable_mysql_branches,
    },
    crd::{
        MirrordClusterOperatorUserCredential, MirrordOperatorCrd, NewOperatorFeature,
        OPERATOR_STATUS_NAME, TargetCrd,
        copy_target::{CopyTargetCrd, CopyTargetSpec, CopyTargetStatus},
        mysql_branching::MysqlBranchDatabase,
    },
    types::{
        CLIENT_CERT_HEADER, CLIENT_HOSTNAME_HEADER, CLIENT_NAME_HEADER, MIRRORD_CLI_VERSION_HEADER,
        SESSION_ID_HEADER,
    },
};

mod conn_wrapper;
mod connect_params;
mod credentials;
mod database_branches;
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
    ///
    /// NOTE: `SpinnerProgress` can interfere with any printed messages coming from interactive
    /// authentication with the cluster, for example via the kubelogin tool
    #[tracing::instrument(level = Level::TRACE, skip_all, err)]
    pub async fn try_new<P, R>(
        config: &LayerConfig,
        reporter: &mut R,
        progress: &P,
    ) -> OperatorApiResult<Option<Self>>
    where
        R: Reporter,
        P: Progress,
    {
        let base_config = Self::base_client_config(config).await?;

        let client = progress
            .suspend(|| ClientBuilder::try_from(base_config.clone()))
            .map_err(KubeApiError::from)?
            .with_layer(&BufferLayer::new(1024))
            .with_layer(&RetryLayer::new(RetryKube::try_from(
                &config.startup_retry,
            )?))
            .build();

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

            // kube api failed to get the operator, let's see if it's installed though
            Err(error @ kube::Error::Api(..)) => {
                match discovery::operator_installed(&client).await {
                    // operator is required, but we failed for some reason
                    Err(..) if config.operator == Some(true) => error,
                    // the operator is not installed, or discovery failed and the operator is not
                    // required
                    Ok(false) | Err(..) => {
                        return Ok(None);
                    }
                    // the operator is there, but we failed getting it
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
    #[tracing::instrument(level = Level::TRACE, skip(reporter, progress))]
    pub async fn prepare_client_cert<P, R>(
        self,
        reporter: &mut R,
        progress: &P,
        layer_config: &LayerConfig,
    ) -> OperatorApi<MaybeClientCert>
    where
        R: Reporter,
        P: Progress,
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

            let client = progress
                .suspend(|| ClientBuilder::try_from(config))
                .map_err(KubeApiError::from)
                .map_err(OperatorApiError::CreateKubeClient)?
                .with_layer(&BufferLayer::new(1024))
                .with_layer(&RetryLayer::new(RetryKube::try_from(
                    &layer_config.startup_retry,
                )?))
                .build();

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
            let no_license_message = "No valid license found for mirrord for Teams. Visit https://app.metalbear.com to purchase or renew your license";
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

    /// Returns a reference to the operator resource fetched from the cluster.
    pub fn operator(&self) -> &MirrordOperatorCrd {
        &self.operator
    }

    /// Returns a reference to the [`Client`] used by this instance.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Create a new CI api key by generating a random key pair, creating a certificate
    /// signing request and sending it to the operator.
    pub async fn create_ci_api_key(&self) -> Result<String, OperatorApiError> {
        if self
            .operator()
            .spec
            .supported_features()
            .contains(&NewOperatorFeature::ExtendableUserCredentials)
            .not()
        {
            return Err(OperatorApiError::UnsupportedFeature {
                feature: NewOperatorFeature::ExtendableUserCredentials,
                operator_version: self.operator().spec.operator_version.to_string(),
            });
        }

        let api_key: CiApiKey = Credentials::init_ci::<MirrordClusterOperatorUserCredential>(
            self.client.clone(),
            &format!(
                "mirrord-ci@{}",
                self.operator.spec.license.organization.as_str()
            ),
        )
        .await
        .map_err(|error| {
            OperatorApiError::ClientCertError(format!(
                "failed to create credentials for CI: {error}"
            ))
        })?
        .into();

        let encoded = api_key.encode().map_err(|error| {
            OperatorApiError::ClientCertError(format!("failed to encode api key: {error}"))
        })?;

        Ok(encoded)
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

        if layer_config
            .feature
            .copy_target
            .exclude_containers
            .is_empty()
            .not()
            || layer_config
                .feature
                .copy_target
                .exclude_init_containers
                .is_empty()
                .not()
        {
            self.operator
                .spec
                .require_feature(NewOperatorFeature::CopyTargetExcludeContainers)?
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
            .get_client_certificate::<MirrordOperatorCrd, MirrordClusterOperatorUserCredential>(
                &self.client,
                fingerprint,
                subscription_id,
                self.operator()
                    .spec
                    .supported_features()
                    .contains(&NewOperatorFeature::ExtendableUserCredentials),
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
        branch_name: Option<String>,
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

        let (do_copy_target, reason) = if layer_config.feature.copy_target.enabled {
            (true, None)
        } else if target.empty_deployment() {
            (true, Some("empty deployment"))
        } else if layer_config.feature.split_queues.sqs().next().is_some()
            && self
                .operator
                .spec
                .supported_features()
                .contains(&NewOperatorFeature::SqsQueueSplittingDirect)
                .not()
        {
            (true, Some("SQS splitting"))
        } else if layer_config.feature.split_queues.kafka().next().is_some()
            && self
                .operator()
                .spec
                .supported_features()
                .contains(&NewOperatorFeature::KafkaQueueSplittingDirect)
                .not()
        {
            (true, Some("Kafka splitting"))
        } else {
            (false, None)
        };

        let mysql_branch_names = if layer_config.feature.db_branches.is_empty().not() {
            Some(self.prepare_branch_dbs(layer_config, progress).await?)
        } else {
            None
        };

        let (session, reused_copy) = if do_copy_target {
            let mut copy_subtask = progress.subtask("preparing target copy");
            if let Some(reason) = reason {
                copy_subtask.info(&format!(
                    "The copy target feature is used for {reason} (even though `copy_target` was not explicitly set)."
                ));
            }

            let (copied, reused) = {
                let reused = self.try_reuse_copy_target(layer_config, progress).await?;
                match reused {
                    Some(reused) => (reused, true),
                    None => (self.copy_target(layer_config, progress).await?, false),
                }
            };
            copy_subtask.success(None);

            let id = copied
                .status
                .as_ref()
                .and_then(|copy_crd| copy_crd.creator_session.id.as_deref());
            let connect_url = Self::copy_target_connect_url(
                &copied,
                use_proxy_api,
                layer_config.profile.as_deref(),
                branch_name.clone(),
                mysql_branch_names.clone().unwrap_or_default(),
            );
            let session = self.make_operator_session(id, connect_url)?;

            (session, reused)
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
                if let Some(modified_ports) = layer_config
                    .feature
                    .network
                    .incoming
                    .add_probe_ports_to_http_filter_ports(&runtime_data.containers_probe_ports)
                {
                    progress.info(&format!("`network.incoming.http_filter.ports` has been set to use ports {modified_ports}."));
                }

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

            let params = ConnectParams::new(
                layer_config,
                branch_name.clone(),
                mysql_branch_names.clone().unwrap_or_default(),
            );
            let connect_url = Self::target_connect_url(use_proxy_api, &target, &params);
            let session = self.make_operator_session(None, connect_url)?;

            (session, false)
        };

        let mut connection_subtask = progress.subtask("connecting to the target");
        let (tx, rx, session) = match Self::connect_target(&self.client, &session).await {
            Ok((tx, rx)) => {
                connection_subtask.success(Some("connected to the target"));
                (tx, rx, session)
            }
            Err(OperatorApiError::KubeError {
                error: kube::Error::Api(response),
                operation: OperatorOperation::WebsocketConnection,
            }) if response.code == 404 && reused_copy => {
                connection_subtask.failure(Some("copied target is gone"));
                let copied = self.copy_target(layer_config, progress).await?;

                let connect_url = Self::copy_target_connect_url(
                    &copied,
                    use_proxy_api,
                    layer_config.profile.as_deref(),
                    branch_name,
                    mysql_branch_names.unwrap_or_default(),
                );
                let session_id = copied
                    .status
                    .as_ref()
                    .and_then(|copy_crd| copy_crd.creator_session.id.as_deref());
                let session = self.make_operator_session(session_id, connect_url)?;

                let mut connection_subtask = progress.subtask("connecting to the target");
                let (tx, rx) = Self::connect_target(&self.client, &session).await?;
                connection_subtask.success(Some("connected to the target"));
                (tx, rx, session)
            }
            Err(error) => return Err(error),
        };

        Ok(OperatorSessionConnection { session, tx, rx })
    }

    /// Creates a new [`OperatorSession`] with the given `id` and `connect_url`.
    ///
    /// If `id` is not passed, a random one is generated.
    #[tracing::instrument(level = Level::DEBUG, err, ret)]
    fn make_operator_session(
        &self,
        id: Option<&str>,
        connect_url: String,
    ) -> OperatorApiResult<OperatorSession> {
        let id = id
            .map(|id| u64::from_str_radix(id, 16))
            .transpose()?
            .unwrap_or_else(rand::random);
        let operator_protocol_version = self
            .operator
            .spec
            .protocol_version
            .as_ref()
            .and_then(|version| version.parse().ok());
        let allow_reconnect = self
            .operator
            .spec
            .supported_features()
            .contains(&NewOperatorFeature::LayerReconnect);

        Ok(OperatorSession {
            id,
            connect_url,
            client_cert: self.client_cert.cert.clone(),
            operator_license_fingerprint: self.operator.spec.license.fingerprint.clone(),
            operator_protocol_version,
            allow_reconnect,
        })
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
        branch_name: Option<String>,
        mysql_branch_names: Vec<String>,
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
            // Kafka and SQS splits are passed in the request body.
            kafka_splits: Default::default(),
            sqs_splits: Default::default(),
            branch_name,
            mysql_branch_names,
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

    /// Creates a new [`CopyTargetCrd`] resource using the operator.
    ///
    /// This should create a new dummy pod out of the [`Target`] specified in the given
    /// [`LayerConfig`].
    ///
    /// # Returns
    ///
    /// The created [`CopyTargetCrd`].
    ///
    /// # Note
    ///
    /// `copy_target` feature is not available for all target types.
    /// Target type compatibility is checked by the operator.
    #[tracing::instrument(level = Level::TRACE, skip(layer_config, progress), err, ret)]
    async fn copy_target<P: Progress>(
        &self,
        layer_config: &LayerConfig,
        progress: &P,
    ) -> OperatorApiResult<CopyTargetCrd> {
        let mut subtask = progress.subtask("copying target");

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
        let split_queues = layer_config
            .feature
            .split_queues
            .is_set()
            .then(|| layer_config.feature.split_queues.clone());

        let exclude_containers = layer_config.feature.copy_target.exclude_containers.clone();
        let exclude_init_containers = layer_config
            .feature
            .copy_target
            .exclude_init_containers
            .clone();

        let copy_target_api: Api<CopyTargetCrd> = Api::namespaced(self.client.clone(), namespace);

        let copy_target_name = TargetCrd::urlfied_name(&target);
        let copy_target_spec = CopyTargetSpec {
            target,
            idle_ttl: Some(Self::COPIED_POD_IDLE_TTL),
            scale_down,
            split_queues,
            exclude_containers,
            exclude_init_containers,
        };

        let copied = copy_target_api
            .create(
                &PostParams::default(),
                &CopyTargetCrd::new(&copy_target_name, copy_target_spec),
            )
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::CopyingTarget,
            })?;
        subtask.success(Some("target copy created"));

        self.wait_for_copy_ready(copied, progress).await
    }

    async fn try_reuse_copy_target<P: Progress>(
        &self,
        layer_config: &LayerConfig,
        progress: &P,
    ) -> OperatorApiResult<Option<CopyTargetCrd>> {
        let mut subtask = progress.subtask("checking for existing target copies");

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
        let split_queues = layer_config
            .feature
            .split_queues
            .is_set()
            .then(|| layer_config.feature.split_queues.clone());

        let exclude_containers = layer_config.feature.copy_target.exclude_containers.clone();
        let exclude_init_containers = layer_config
            .feature
            .copy_target
            .exclude_init_containers
            .clone();

        let user_id = self.get_user_id_str();

        let copy_target_api: Api<CopyTargetCrd> = Api::namespaced(self.client.clone(), namespace);
        let copy_target_spec = CopyTargetSpec {
            target,
            idle_ttl: Some(Self::COPIED_POD_IDLE_TTL),
            scale_down,
            split_queues,
            exclude_containers,
            exclude_init_containers,
        };

        let existing = copy_target_api
            .list(&ListParams::default())
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::CopyingTarget,
            })?
            .items
            .into_iter()
            .find(|copy_target| {
                copy_target.spec == copy_target_spec
                    && copy_target.status.as_ref().is_some_and(|status| {
                        status.creator_session.user_id.as_ref() == Some(&user_id)
                            && status.phase.as_deref() != Some(CopyTargetStatus::PHASE_FAILED)
                    })
            });

        let existing = match existing {
            Some(existing) => match self.wait_for_copy_ready(existing, progress).await {
                Ok(copied) => Some(copied),
                Err(OperatorApiError::CopiedTargetFailed { .. }) => None,
                Err(error) => return Err(error),
            },
            None => None,
        };

        if existing.is_some() {
            subtask.success(Some("found an existing copy"));
        } else {
            subtask.failure(Some("no existing copy was found"));
        }

        Ok(existing)
    }

    /// Polls the given [`CopyTargetCrd`] for readiness.
    async fn wait_for_copy_ready<P: Progress>(
        &self,
        mut copied: CopyTargetCrd,
        progress: &P,
    ) -> OperatorApiResult<CopyTargetCrd> {
        let namespace = copied
            .metadata
            .namespace
            .as_deref()
            .ok_or_else(|| KubeApiError::invalid_state(&copied, "no namespace"))?;
        let name = copied
            .metadata
            .name
            .clone()
            .ok_or_else(|| KubeApiError::invalid_state(&copied, "no name"))?;
        let api = Api::<CopyTargetCrd>::namespaced(self.client.clone(), namespace);
        let mut wait_subtask: Option<P> = None;

        loop {
            let phase = copied
                .status
                .as_ref()
                .and_then(|status| status.phase.as_deref());
            match phase {
                Some(CopyTargetStatus::PHASE_IN_PROGRESS) => {
                    if wait_subtask.is_none() {
                        wait_subtask.replace(progress.subtask("waiting for the copy to be ready"));
                    }
                }
                Some(CopyTargetStatus::PHASE_READY) | None => {
                    if let Some(mut subtask) = wait_subtask {
                        subtask.success(None);
                    }
                    break Ok(copied);
                }
                Some(CopyTargetStatus::PHASE_FAILED) => {
                    break Err(OperatorApiError::CopiedTargetFailed {
                        message: copied.status.and_then(|status| status.failure_message),
                    });
                }
                Some(other) => {
                    break Err(OperatorApiError::CopiedTargetFailed {
                        message: Some(format!("unknown phase `{other}`")),
                    });
                }
            }

            tokio::time::sleep(Duration::from_secs(5)).await;
            copied = api
                .get(&name)
                .await
                .map_err(|error| OperatorApiError::KubeError {
                    error,
                    operation: OperatorOperation::CopyingTarget,
                })?;
        }
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

        let client = ClientBuilder::try_from(config)
            .map_err(KubeApiError::from)
            .map_err(OperatorApiError::CreateKubeClient)?
            .with_layer(&BufferLayer::new(1024))
            .with_layer(&RetryLayer::new(RetryKube::try_from(
                &layer_config.startup_retry,
            )?))
            .build();

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

    /// Prepare branch databases, and return database resource names.
    ///
    /// 1. List reusable branch databases.
    /// 2. Create new ones if any missing.
    /// 3. Wait for all new databases to be ready.
    #[tracing::instrument(level = Level::TRACE, skip_all, err, ret)]
    async fn prepare_branch_dbs<P: Progress>(
        &self,
        layer_config: &LayerConfig,
        progress: &P,
    ) -> OperatorApiResult<Vec<String>> {
        let mut subtask = progress.subtask("preparing branch databases");
        let target = layer_config
            .target
            .path
            .clone()
            .unwrap_or(Target::Targetless);
        let namespace = layer_config
            .target
            .namespace
            .as_deref()
            .unwrap_or(self.client.default_namespace());
        let mysql_branch_api: Api<MysqlBranchDatabase> =
            Api::namespaced(self.client.clone(), namespace);

        let DatabaseBranchParams {
            mysql: mut create_mysql_params,
        } = DatabaseBranchParams::new(&layer_config.feature.db_branches, &target);

        let reusable_mysql_branches =
            list_reusable_mysql_branches(&mysql_branch_api, &create_mysql_params, &subtask).await?;

        create_mysql_params.retain(|id, _| !reusable_mysql_branches.contains_key(id));
        let created_mysql_branches =
            create_mysql_branches(&mysql_branch_api, create_mysql_params, &subtask).await?;
        subtask.success(None);

        let mysql_branch_names = reusable_mysql_branches
            .values()
            .chain(created_mysql_branches.values())
            .map(|branch| {
                branch
                    .meta()
                    .name
                    .clone()
                    .ok_or(KubeApiError::missing_field(branch, ".metadata.name"))
            })
            .collect::<Result<Vec<String>, _>>()?;
        Ok(mysql_branch_names)
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
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
        Default::default(),
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
        Default::default(),
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
        Default::default(),
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
        Default::default(),
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
        Default::default(),
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
        Default::default(),
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
        Default::default(),
        Default::default(),
        "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv\
        ?connect=true&on_concurrent_steal=abort&kafka_splits=%7B%22topic-id%22%3A%7B%22header-1%22%3A%22filter-1%22%2C%22header-2%22%3A%22filter-2%22%7D%7D",
    )]
    #[case::deployment_container_proxy_sqs_splits(
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
        HashMap::from([(
            "topic-id",
            BTreeMap::from([
                ("header-1".to_string(), "filter-1".to_string()),
                ("header-2".to_string(), "filter-2".to_string()),
            ]),
        )]),
        Default::default(),
        "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv\
        ?connect=true&on_concurrent_steal=abort&sqs_splits=%7B%22topic-id%22%3A%7B%22header-1%22%3A%22filter-1%22%2C%22header-2%22%3A%22filter-2%22%7D%7D",
    )]
    #[case::deployment_container_proxy_mysql_branches(
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
        Default::default(),
        vec!["branch-1".into(), "branch-2".into()],
        "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv\
        ?connect=true&on_concurrent_steal=abort&mysql_branch_names=%5B%22branch-1%22%2C%22branch-2%22%5D",
    )]
    #[test]
    fn target_connect_url(
        #[case] use_proxy: bool,
        #[case] target: ResolvedTarget<true>,
        #[case] concurrent_steal: ConcurrentSteal,
        #[case] profile: Option<&str>,
        #[case] kafka_splits: HashMap<&str, BTreeMap<String, String>>,
        #[case] sqs_splits: HashMap<&str, BTreeMap<String, String>>,
        #[case] mysql_branch_names: Vec<String>,
        #[case] expected: &str,
    ) {
        let kafka_splits = kafka_splits
            .iter()
            .map(|(topic_id, filters)| (*topic_id, filters))
            .collect();

        let sqs_splits = sqs_splits
            .iter()
            .map(|(topic_id, filters)| (*topic_id, filters))
            .collect();

        let params = ConnectParams {
            connect: true,
            on_concurrent_steal: Some(concurrent_steal),
            profile,
            kafka_splits,
            sqs_splits,
            branch_name: None,
            mysql_branch_names,
        };

        let produced = OperatorApi::target_connect_url(use_proxy, &target, &params);
        assert_eq!(produced, expected)
    }
}
