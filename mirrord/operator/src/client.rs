use std::{fmt, ops::Not, time::Duration};

use base64::{Engine, engine::general_purpose};
use chrono::{DateTime, Utc};
use connect_params::ConnectParams;
use error::{OperatorApiError, OperatorApiResult, OperatorOperation};
use futures::{SinkExt, StreamExt, future::Either};
use http::{HeaderName, HeaderValue, request::Request};
use k8s_openapi::api::apps::v1::Deployment;
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
use mirrord_config::{
    LayerConfig, feature::database_branches::default_creation_timeout_secs, target::Target,
};
use mirrord_kube::{
    api::{
        kubernetes::{
            create_kube_config,
            rollout::{Rollout, RolloutSpec, workload_ref::WorkloadRef},
        },
        runtime::RuntimeDataProvider,
    },
    error::KubeApiError,
    resolved::{ResolvedResource, ResolvedTarget},
    retry::RetryKube,
};
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use mirrord_protocol_io::{Client as ProtocolClient, Connection};
use semver::Version;
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite;
use tower::{buffer::BufferLayer, retry::RetryLayer};
use tracing::Level;

use crate::{
    client::database_branches::{
        DatabaseBranchParams, create_mongodb_branches, create_mysql_branches, create_pg_branches,
        list_reusable_mongodb_branches, list_reusable_mysql_branches, list_reusable_pg_branches,
    },
    crd::{
        MirrordClusterOperatorUserCredential, MirrordOperatorCrd, NewOperatorFeature,
        OPERATOR_STATUS_NAME, TargetCrd,
        copy_target::{CopyTargetCrd, CopyTargetSpec, CopyTargetStatus},
        mongodb_branching::MongodbBranchDatabase,
        mysql_branching::MysqlBranchDatabase,
        pg_branching::PgBranchDatabase,
        session::SessionCiInfo,
    },
    types::{
        CLIENT_CERT_HEADER, CLIENT_HOSTNAME_HEADER, CLIENT_NAME_HEADER, MIRRORD_CLI_VERSION_HEADER,
        SESSION_ID_HEADER,
    },
};

pub mod connect_params;
mod credentials;
pub mod database_branches;
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
    /// Version of the operator, right now only for [`fmt::Debug`] implementation.
    operator_version: Version,
    /// Version of [`mirrord_protocol`] used by the operator.
    /// Used to create [`Connection`].
    pub operator_protocol_version: Option<Version>,
    /// Allow the layer to attempt reconnection
    pub allow_reconnect: bool,
    /// OpenTelemetry (OTel) / W3C trace context.
    /// See [OTel docs](https://opentelemetry.io/docs/specs/otel/context/env-carriers/#environment-variable-names)
    traceparent: Option<String>,
    /// OpenTelemetry (OTel) / W3C baggage propagator.
    /// See [OTel docs](https://opentelemetry.io/docs/specs/otel/context/env-carriers/#environment-variable-names)
    baggage: Option<String>,
}

impl fmt::Debug for OperatorSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug_struct = f.debug_struct("OperatorSession");
        debug_struct
            .field("id", &format!("{:X}", self.id))
            .field("connect_url", &self.connect_url)
            .field("cert_public_key_data", &self.client_cert.public_key_data())
            .field(
                "operator_license_fingerprint",
                &self.operator_license_fingerprint,
            )
            .field("operator_protocol_version", &self.operator_protocol_version)
            .field("operator_version", &self.operator_version)
            .field("allow_reconnect", &self.allow_reconnect)
            .field("traceparent", &self.traceparent)
            .field("baggage", &self.baggage);
        debug_struct.finish()
    }
}

/// Connection to an operator target.
pub struct OperatorSessionConnection {
    pub session: Box<OperatorSession>,
    pub conn: Connection<ProtocolClient>,
}

impl fmt::Debug for OperatorSessionConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OperatorSessionConnection")
            .field("session", &self.session)
            .field("closed", &self.conn.is_closed())
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

    #[tracing::instrument(level = Level::TRACE, skip(reporter, progress))]
    pub async fn with_ci_api_key<P, R>(
        self,
        reporter: &mut R,
        progress: &P,
        layer_config: &LayerConfig,
        ci_api_key: &CiApiKey,
    ) -> OperatorApi<MaybeClientCert>
    where
        R: Reporter,
        P: Progress,
    {
        let certificate = ci_api_key.credentials().as_ref();

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

        self.prepare_with_certificate(progress, layer_config, certificate)
            .await
    }

    #[tracing::instrument(level = Level::TRACE, skip(progress))]
    async fn prepare_with_certificate<P>(
        self,
        progress: &P,
        layer_config: &LayerConfig,
        certificate: &Certificate,
    ) -> OperatorApi<MaybeClientCert>
    where
        P: Progress,
    {
        let previous_client = self.client.clone();

        let result = try {
            let header = Self::make_client_cert_header(certificate)?;

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
                    cert_result: Ok(cert.clone()),
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

    /// Prepares client [`Certificate`] to be sent in all subsequent requests to the operator.
    /// In case of failure, state of this API instance does not change.
    #[tracing::instrument(level = Level::TRACE, skip(reporter, progress))]
    pub async fn with_client_certificate<P, R>(
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
        let operator_crd = self.operator.clone();

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

            self.prepare_with_certificate(progress, layer_config, &certificate)
                .await
        };

        match result {
            Ok(api) => api,
            Err(error) => OperatorApi {
                client: previous_client,
                client_cert: MaybeClientCert {
                    cert_result: Err(error),
                },
                operator: operator_crd,
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

        let credentials = Credentials::init_ci::<MirrordClusterOperatorUserCredential>(
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
        })?;

        let api_key = CiApiKey::V1(credentials);

        let encoded = api_key.encode_as_url_safe_string().map_err(|error| {
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
        session_ci_info: Option<SessionCiInfo>,
    ) -> OperatorApiResult<OperatorSessionConnection>
    where
        P: Progress,
    {
        self.check_feature_support(layer_config)?;
        let (do_copy_target, reason) = self
            .should_copy_target(layer_config, &target, progress)
            .await?;

        let use_proxy_api = self
            .operator
            .spec
            .supported_features()
            .contains(&NewOperatorFeature::ProxyApi);

        let mysql_branch_names = if layer_config.feature.db_branches.is_empty().not() {
            Some(
                self.prepare_mysql_branch_dbs(layer_config, progress)
                    .await?,
            )
        } else {
            None
        };
        let pg_branch_names = if layer_config.feature.db_branches.is_empty().not() {
            Some(self.prepare_pg_branch_dbs(layer_config, progress).await?)
        } else {
            None
        };
        let mongodb_branch_names = if layer_config.feature.db_branches.is_empty().not() {
            Some(
                self.prepare_mongodb_branch_dbs(layer_config, progress)
                    .await?,
            )
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
                mongodb_branch_names.clone().unwrap_or_default(),
                mysql_branch_names.clone().unwrap_or_default(),
                pg_branch_names.clone().unwrap_or_default(),
                session_ci_info.clone(),
                layer_config.key.as_str(),
            );
            let session = self.make_operator_session(
                id,
                connect_url,
                layer_config.traceparent.clone(),
                layer_config.baggage.clone(),
            )?;

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
                mongodb_branch_names.clone().unwrap_or_default(),
                mysql_branch_names.clone().unwrap_or_default(),
                pg_branch_names.clone().unwrap_or_default(),
                session_ci_info.clone(),
                layer_config.key.as_str(),
            );
            let connect_url = Self::target_connect_url(use_proxy_api, &target, &params);

            let session = self.make_operator_session(
                None,
                connect_url,
                layer_config.traceparent.clone(),
                layer_config.baggage.clone(),
            )?;

            (session, false)
        };

        let mut connection_subtask = progress.subtask("connecting to the target");
        let (conn, session) = match Self::connect_target(&self.client, &session).await {
            Ok(conn) => {
                connection_subtask.success(Some("connected to the target"));
                (conn, session)
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
                    mongodb_branch_names.unwrap_or_default(),
                    mysql_branch_names.unwrap_or_default(),
                    pg_branch_names.unwrap_or_default(),
                    session_ci_info.clone(),
                    layer_config.key.as_str(),
                );
                let session_id = copied
                    .status
                    .as_ref()
                    .and_then(|copy_crd| copy_crd.creator_session.id.as_deref());
                let session = self.make_operator_session(
                    session_id,
                    connect_url,
                    layer_config.traceparent.clone(),
                    layer_config.baggage.clone(),
                )?;

                let mut connection_subtask = progress.subtask("connecting to the target");
                let conn = Self::connect_target(&self.client, &session).await?;
                connection_subtask.success(Some("connected to the target"));
                (conn, session)
            }
            Err(error) => return Err(error),
        };

        Ok(OperatorSessionConnection {
            session: Box::new(session),
            conn,
        })
    }

    /// Connect to operator using target config directly (no K8s resolution).
    ///
    /// Used when the target may not exist locally, e.g., in multi-cluster mode
    /// where the primary cluster is management-only and targets exist only on
    /// workload clusters. The operator handles target resolution on the appropriate cluster.
    ///
    /// This method skips:
    /// - `assert_valid_mirrord_target` (operator validates on workload cluster)
    /// - `runtime_data` warnings (operator handles on workload cluster)
    /// - `copy_target` (not supported without local resolution)
    pub async fn connect_in_multi_cluster_session<P>(
        &self,
        target: &Target,
        layer_config: &mut LayerConfig,
        progress: &P,
        branch_name: Option<String>,
        session_ci_info: Option<SessionCiInfo>,
    ) -> OperatorApiResult<OperatorSessionConnection>
    where
        P: Progress,
    {
        use mirrord_config::target::TargetDisplay;

        let namespace = layer_config.target.namespace.as_deref();

        tracing::info!(
            target_type = %target.type_(),
            target_name = %target.name(),
            namespace = ?namespace,
            "Connecting to multi-cluster primary - workload cluster will resolve target"
        );

        let use_proxy_api = self
            .operator
            .spec
            .supported_features()
            .contains(&NewOperatorFeature::ProxyApi);

        // Create branch CRDs on the cluster CLI has access to:
        // - Single-cluster: CRDs created directly on target cluster, branching happens on the entry
        //   cluster.
        // - Multi-cluster: CRDs created on primary cluster. If Primary == Default, branching
        //   happens locally. If Primary != Default, a sync controller copies CRDs to Default where
        //   actual branching happens, then syncs status back.
        let mysql_branch_names = if layer_config.feature.db_branches.is_empty().not() {
            self.prepare_mysql_branch_dbs(layer_config, progress)
                .await?
        } else {
            vec![]
        };
        let pg_branch_names = if layer_config.feature.db_branches.is_empty().not() {
            self.prepare_pg_branch_dbs(layer_config, progress).await?
        } else {
            vec![]
        };
        let mongodb_branch_names = if layer_config.feature.db_branches.is_empty().not() {
            self.prepare_mongodb_branch_dbs(layer_config, progress)
                .await?
        } else {
            vec![]
        };

        let params = ConnectParams::new(
            layer_config,
            branch_name,
            mongodb_branch_names,
            mysql_branch_names,
            pg_branch_names,
            session_ci_info,
            layer_config.key.as_str(),
        );

        // If no namespace in config, use the kubeconfig's default namespace
        let namespace = namespace.unwrap_or_else(|| self.client.default_namespace());
        let connect_url =
            Self::target_connect_url_from_config(use_proxy_api, target, namespace, &params);

        let session = self.make_operator_session(
            None,
            connect_url,
            layer_config.traceparent.clone(),
            layer_config.baggage.clone(),
        )?;

        let mut connection_subtask = progress.subtask("connecting to the target");
        let conn = Self::connect_target(&self.client, &session).await?;
        connection_subtask.success(Some("connected to the target"));

        Ok(OperatorSessionConnection {
            session: Box::new(session),
            conn,
        })
    }

    /// Returns whether the `copy_target` feature should be used,
    /// with an optional reason (in case the feature is not explicitly enabled in the
    /// [`LayerConfig`]).
    async fn should_copy_target<P: Progress>(
        &self,
        config: &LayerConfig,
        target: &ResolvedTarget<false>,
        progress: &P,
    ) -> OperatorApiResult<(bool, Option<&'static str>)> {
        if config.feature.copy_target.enabled {
            // Explicitly enabled.
            return Ok((true, None));
        }

        if config.feature.split_queues.sqs().next().is_some()
            && self
                .operator
                .spec
                .supported_features()
                .contains(&NewOperatorFeature::SqsQueueSplittingDirect)
                .not()
        {
            // Operator does not support SQS splitting without copying the target.
            return Ok((true, Some("SQS splitting")));
        }

        if config.feature.split_queues.kafka().next().is_some()
            && self
                .operator()
                .spec
                .supported_features()
                .contains(&NewOperatorFeature::KafkaQueueSplittingDirect)
                .not()
        {
            // Operator does not support Kafka splitting without copying the target.
            return Ok((true, Some("Kafka splitting")));
        }

        let ResolvedTarget::Deployment(ResolvedResource { resource, .. }) = target else {
            // We do replicas checks only for deployments.
            return Ok((false, None));
        };

        let available_replicas = resource
            .status
            .as_ref()
            .and_then(|status| status.available_replicas)
            .unwrap_or_default();
        if available_replicas > 0 {
            // Has available replicas, all good.
            return Ok((false, None));
        }

        let replicas = resource
            .spec
            .as_ref()
            .and_then(|spec| spec.replicas)
            .unwrap_or(1);
        if replicas > 0 {
            // Is configured to have replicas.
            // We assume that the deployment is in bad state only temporarily,
            // and enable copy_target to improve UX.
            return Ok((true, Some("empty deployment")));
        }

        let deploy_name = resource
            .metadata
            .name
            .as_deref()
            .ok_or_else(|| KubeApiError::missing_field(resource.as_ref(), ".metadata.name"))?;
        let deploy_namespace =
            resource.metadata.namespace.as_deref().ok_or_else(|| {
                KubeApiError::missing_field(resource.as_ref(), ".metadata.namespace")
            })?;
        let api = Api::<Rollout>::namespaced(self.client.clone(), deploy_namespace);
        match api.get(deploy_name).await {
            // There is a rollout managing the target deployment via workload ref.
            // The user should target the rollout instead.
            Ok(Rollout {
                spec:
                    Some(RolloutSpec {
                        workload_ref:
                            Some(WorkloadRef {
                                api_version,
                                kind,
                                name,
                            }),
                        ..
                    }),
                ..
            }) if api_version == Deployment::api_version(&())
                && kind == Deployment::kind(&())
                && name == deploy_name =>
            {
                return Err(OperatorApiError::KubeApi(KubeApiError::invalid_state(
                    resource.as_ref(),
                    "deployment is an empty workload managed by a rollout with the same name, \
                    please target the rollout instead",
                )));
            }
            // There is a rollout with the same name, but it does not manage the target deployment.
            // This is weird, logging it on debug.
            Ok(rollout) => {
                tracing::debug!(
                    deployment = ?resource,
                    rollout = ?rollout,
                    "Target deployment is empty, and a rollout with the same name was found in the target namespace. \
                    However, the rollout does not manage the deployment. \
                    Copying the target for this session.",
                );
            }
            // The rollout does not exist.
            // It might be that rollouts are not even installed in the cluster.
            // Depeneding on how hardened the cluster is, we might get one of these error codes.
            Err(kube::Error::Api(response)) if [404, 403, 401].contains(&response.code) => {}
            Err(error) => {
                tracing::warn!(
                    deployment = ?resource,
                    %error,
                    "Failed to check if the targeted empty deployment is managed by a rollout. \
                    Copying the target for this session.",
                );
                progress.warning(&format!(
                    "The target deployment is empty, and mirrord failed to check if it is managed by a rollout. \
                    This session will use the copy_target feature. Error: {error}",
                ));
            }
        }

        Ok((true, Some("empty deployment")))
    }

    /// Creates a new [`OperatorSession`] with the given `id` and `connect_url`.
    ///
    /// If `id` is not passed, a random one is generated.
    #[tracing::instrument(level = Level::DEBUG, err, ret)]
    fn make_operator_session(
        &self,
        id: Option<&str>,
        connect_url: String,
        traceparent: Option<String>,
        baggage: Option<String>,
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
        let operator_version = self.operator.spec.operator_version.clone();
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
            operator_version,
            allow_reconnect,
            traceparent,
            baggage,
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

    /// Produces the URL from Target config directly (no K8s resolution needed).
    /// Used when the target may not exist locally (e.g., multi-cluster with management-only
    /// primary).
    fn target_connect_url_from_config(
        use_proxy: bool,
        target: &Target,
        namespace: &str,
        connect_params: &ConnectParams<'_>,
    ) -> String {
        use mirrord_config::target::TargetDisplay;

        let name = {
            let mut urlfied_name = target.type_().to_string();
            // For targetless, name() returns "targetless" which would result in
            // "targetless.targetless" - so we skip this
            if !matches!(target, Target::Targetless) {
                urlfied_name.push('.');
                urlfied_name.push_str(target.name());
                if let Some(container) = target.container() {
                    urlfied_name.push_str(".container.");
                    urlfied_name.push_str(container);
                }
            }
            urlfied_name
        };

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
    #[allow(clippy::too_many_arguments)]
    fn copy_target_connect_url(
        crd: &CopyTargetCrd,
        use_proxy: bool,
        profile: Option<&str>,
        branch_name: Option<String>,
        mongodb_branch_names: Vec<String>,
        mysql_branch_names: Vec<String>,
        pg_branch_names: Vec<String>,
        session_ci_info: Option<SessionCiInfo>,
        key: &str,
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
            mongodb_branch_names,
            mysql_branch_names,
            pg_branch_names,
            session_ci_info,
            is_default_cluster: None, // Only used in multi-cluster
            sqs_output_queues: Default::default(), // Only used in multi-cluster
            key: Some(key),
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
        session: Box<OperatorSession>,
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

        let conn = Self::connect_target(&client, &session).await?;

        Ok(OperatorSessionConnection { conn, session })
    }

    /// Creates websocket connection to the operator target.
    #[tracing::instrument(level = Level::TRACE, skip(client), err)]
    async fn connect_target(
        client: &Client,
        session: &OperatorSession,
    ) -> OperatorApiResult<Connection<ProtocolClient>> {
        let request_builder = Request::builder()
            .uri(&session.connect_url)
            .header(SESSION_ID_HEADER, session.id.to_string());
        let request_builder = if let Some(traceparent) = &session.traceparent {
            request_builder.header("traceparent", traceparent.clone())
        } else {
            request_builder
        };
        let request_builder = if let Some(baggage) = &session.baggage {
            request_builder.header("baggage", baggage.clone())
        } else {
            request_builder
        };

        let request = request_builder
            .body(vec![])
            .map_err(OperatorApiError::ConnectRequestBuildError)?;

        #[derive(thiserror::Error, Debug)]
        enum OperatorClientError {
            #[error(transparent)]
            DecodeError(#[from] bincode::error::DecodeError),
            #[error(transparent)]
            WsError(#[from] tungstenite::Error),
            #[error("invalid message: {0:?}")]
            InvalidMessage(tungstenite::Message),
        }

        let ws = upgrade::connect_ws(client, request)
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::WebsocketConnection,
            })?
            .with(|e: Vec<u8>| async {
                Ok::<_, OperatorClientError>(tungstenite::Message::Binary(e))
            })
            .map(|i| match i.map_err(OperatorClientError::from)? {
                tungstenite::Message::Binary(pl) => Ok(pl),
                other => Err(OperatorClientError::InvalidMessage(other)),
            });

        let operator_protocol_version = session.operator_protocol_version.clone();

        let conn = Connection::<ProtocolClient>::from_channel(
            ws,
            // Mock protocol version negotiation if the operator does not support it.
            Some(move |msg| match msg {
                ClientMessage::SwitchProtocolVersion(version) => match &operator_protocol_version {
                    Some(operator_protocol_version) => {
                        Either::Left(ClientMessage::SwitchProtocolVersion(
                            operator_protocol_version.min(&version).clone(),
                        ))
                    }
                    _ => Either::Right(DaemonMessage::SwitchProtocolVersionResponse(
                        semver::Version::new(1, 2, 1),
                    )),
                },
                other => Either::Left(other),
            }),
        )
        .await?;

        Ok(conn)
    }

    /// Prepare branch databases, and return database resource names.
    ///
    /// 1. List reusable branch databases.
    /// 2. Create new ones if any missing.
    /// 3. Wait for all new databases to be ready.
    #[tracing::instrument(level = Level::TRACE, skip_all, err, ret)]
    async fn prepare_mysql_branch_dbs<P: Progress>(
        &self,
        layer_config: &LayerConfig,
        progress: &P,
    ) -> OperatorApiResult<Vec<String>> {
        use database_branches::TARGET_NAMESPACE_ANNOTATION;

        let mut subtask = progress.subtask("preparing branch databases");
        let target = layer_config
            .target
            .path
            .clone()
            .unwrap_or(Target::Targetless);
        let target_namespace = layer_config
            .target
            .namespace
            .as_deref()
            .unwrap_or(self.client.default_namespace());

        // In multi-cluster management-only mode, create CRDs in operator's namespace
        // with an annotation specifying the target namespace for the sync controller.
        let (api_namespace, target_ns_annotation) =
            if let Some(op_ns) = &self.operator.spec.operator_namespace {
                (op_ns.as_str(), Some(target_namespace.to_string()))
            } else {
                (target_namespace, None)
            };

        let mysql_branch_api: Api<MysqlBranchDatabase> =
            Api::namespaced(self.client.clone(), api_namespace);
        let DatabaseBranchParams {
            mongodb: _create_mongodb_params,
            mysql: mut create_mysql_params,
            pg: _create_pg_params,
        } = DatabaseBranchParams::new(&layer_config.feature.db_branches, &target);

        // Add target namespace annotation if using operator namespace
        if let Some(ref ns) = target_ns_annotation {
            for params in create_mysql_params.values_mut() {
                params
                    .annotations
                    .insert(TARGET_NAMESPACE_ANNOTATION.to_string(), ns.clone());
            }
        }

        let reusable_mysql_branches =
            list_reusable_mysql_branches(&mysql_branch_api, &create_mysql_params, &subtask).await?;

        create_mysql_params.retain(|id, _| !reusable_mysql_branches.contains_key(id));

        // Get the maximum timeout from all DB branch configs
        let timeout_secs = layer_config
            .feature
            .db_branches
            .iter()
            .filter_map(|branch_config| match branch_config {
                mirrord_config::feature::database_branches::DatabaseBranchConfig::Mongodb(
                    mongodb_config,
                ) => Some(mongodb_config.base.creation_timeout_secs),
                mirrord_config::feature::database_branches::DatabaseBranchConfig::Mysql(
                    mysql_config,
                ) => Some(mysql_config.base.creation_timeout_secs),
                mirrord_config::feature::database_branches::DatabaseBranchConfig::Pg(pg_config) => {
                    Some(pg_config.base.creation_timeout_secs)
                }
                mirrord_config::feature::database_branches::DatabaseBranchConfig::Redis(_) => None,
            })
            .max()
            .unwrap_or(default_creation_timeout_secs());
        let timeout = std::time::Duration::from_secs(timeout_secs);

        let created_mysql_branches =
            create_mysql_branches(&mysql_branch_api, create_mysql_params, timeout, &subtask)
                .await?;

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

    /// Prepare branch databases, and return database resource names.
    ///
    /// 1. List reusable branch databases.
    /// 2. Create new ones if any missing.
    /// 3. Wait for all new databases to be ready.
    #[tracing::instrument(level = Level::TRACE, skip_all, err, ret)]
    async fn prepare_pg_branch_dbs<P: Progress>(
        &self,
        layer_config: &LayerConfig,
        progress: &P,
    ) -> OperatorApiResult<Vec<String>> {
        use database_branches::TARGET_NAMESPACE_ANNOTATION;

        let mut subtask = progress.subtask("preparing branch databases");
        let target = layer_config
            .target
            .path
            .clone()
            .unwrap_or(Target::Targetless);
        let target_namespace = layer_config
            .target
            .namespace
            .as_deref()
            .unwrap_or(self.client.default_namespace());

        // In multi-cluster management-only mode, create CRDs in operator's namespace
        // with an annotation specifying the target namespace for the sync controller.
        let (api_namespace, target_ns_annotation) =
            if let Some(op_ns) = &self.operator.spec.operator_namespace {
                (op_ns.as_str(), Some(target_namespace.to_string()))
            } else {
                (target_namespace, None)
            };

        let pg_branch_api: Api<PgBranchDatabase> =
            Api::namespaced(self.client.clone(), api_namespace);
        let DatabaseBranchParams {
            mongodb: _create_mongodb_params,
            mysql: _create_mysql_params,
            pg: mut create_pg_params,
        } = DatabaseBranchParams::new(&layer_config.feature.db_branches, &target);

        // Add target namespace annotation if using operator namespace
        if let Some(ref ns) = target_ns_annotation {
            for params in create_pg_params.values_mut() {
                params
                    .annotations
                    .insert(TARGET_NAMESPACE_ANNOTATION.to_string(), ns.clone());
            }
        }

        let reusable_pg_branches =
            list_reusable_pg_branches(&pg_branch_api, &create_pg_params, &subtask).await?;

        create_pg_params.retain(|id, _| !reusable_pg_branches.contains_key(id));

        // Get the maximum timeout from all DB branch configs
        let timeout_secs = layer_config
            .feature
            .db_branches
            .iter()
            .filter_map(|branch_config| match branch_config {
                mirrord_config::feature::database_branches::DatabaseBranchConfig::Mongodb(
                    mongodb_config,
                ) => Some(mongodb_config.base.creation_timeout_secs),
                mirrord_config::feature::database_branches::DatabaseBranchConfig::Mysql(
                    mysql_config,
                ) => Some(mysql_config.base.creation_timeout_secs),
                mirrord_config::feature::database_branches::DatabaseBranchConfig::Pg(pg_config) => {
                    Some(pg_config.base.creation_timeout_secs)
                }
                mirrord_config::feature::database_branches::DatabaseBranchConfig::Redis(_) => None,
            })
            .max()
            .unwrap_or(default_creation_timeout_secs());
        let timeout = std::time::Duration::from_secs(timeout_secs);

        let created_pg_branches =
            create_pg_branches(&pg_branch_api, create_pg_params, timeout, &subtask).await?;

        subtask.success(None);

        let pg_branch_names = reusable_pg_branches
            .values()
            .chain(created_pg_branches.values())
            .map(|branch| {
                branch
                    .meta()
                    .name
                    .clone()
                    .ok_or(KubeApiError::missing_field(branch, ".metadata.name"))
            })
            .collect::<Result<Vec<String>, _>>()?;

        Ok(pg_branch_names)
    }

    /// Prepare MongoDB branch databases, and return database resource names.
    ///
    /// 1. List reusable branch databases.
    /// 2. Create new ones if any missing.
    /// 3. Wait for all new databases to be ready.
    #[tracing::instrument(level = Level::TRACE, skip_all, err, ret)]
    async fn prepare_mongodb_branch_dbs<P: Progress>(
        &self,
        layer_config: &LayerConfig,
        progress: &P,
    ) -> OperatorApiResult<Vec<String>> {
        use database_branches::TARGET_NAMESPACE_ANNOTATION;

        let mut subtask = progress.subtask("preparing MongoDB branch databases");
        let target = layer_config
            .target
            .path
            .clone()
            .unwrap_or(Target::Targetless);
        let target_namespace = layer_config
            .target
            .namespace
            .as_deref()
            .unwrap_or(self.client.default_namespace());

        // In multi-cluster management-only mode, create CRDs in operator's namespace
        // with an annotation specifying the target namespace for the sync controller.
        let (api_namespace, target_ns_annotation) =
            if let Some(op_ns) = &self.operator.spec.operator_namespace {
                (op_ns.as_str(), Some(target_namespace.to_string()))
            } else {
                (target_namespace, None)
            };

        let mongodb_branch_api: Api<MongodbBranchDatabase> =
            Api::namespaced(self.client.clone(), api_namespace);
        let DatabaseBranchParams {
            mongodb: mut create_mongodb_params,
            mysql: _create_mysql_params,
            pg: _create_pg_params,
        } = DatabaseBranchParams::new(&layer_config.feature.db_branches, &target);

        // Add target namespace annotation if using operator namespace
        if let Some(ref ns) = target_ns_annotation {
            for params in create_mongodb_params.values_mut() {
                params
                    .annotations
                    .insert(TARGET_NAMESPACE_ANNOTATION.to_string(), ns.clone());
            }
        }

        let reusable_mongodb_branches =
            list_reusable_mongodb_branches(&mongodb_branch_api, &create_mongodb_params, &subtask)
                .await?;

        create_mongodb_params.retain(|id, _| !reusable_mongodb_branches.contains_key(id));

        // Get the maximum timeout from all DB branch configs
        let timeout_secs = layer_config
            .feature
            .db_branches
            .iter()
            .filter_map(|branch_config| match branch_config {
                mirrord_config::feature::database_branches::DatabaseBranchConfig::Mongodb(
                    mongodb_config,
                ) => Some(mongodb_config.base.creation_timeout_secs),
                mirrord_config::feature::database_branches::DatabaseBranchConfig::Mysql(
                    mysql_config,
                ) => Some(mysql_config.base.creation_timeout_secs),
                mirrord_config::feature::database_branches::DatabaseBranchConfig::Pg(pg_config) => {
                    Some(pg_config.base.creation_timeout_secs)
                }
                mirrord_config::feature::database_branches::DatabaseBranchConfig::Redis(_) => None,
            })
            .max()
            .unwrap_or(default_creation_timeout_secs());
        let timeout = std::time::Duration::from_secs(timeout_secs);

        let created_mongodb_branches = create_mongodb_branches(
            &mongodb_branch_api,
            create_mongodb_params,
            timeout,
            &subtask,
        )
        .await?;

        subtask.success(None);

        let mongodb_branch_names = reusable_mongodb_branches
            .values()
            .chain(created_mongodb_branches.values())
            .map(|branch| {
                branch
                    .meta()
                    .name
                    .clone()
                    .ok_or(KubeApiError::missing_field(branch, ".metadata.name"))
            })
            .collect::<Result<Vec<String>, _>>()?;

        Ok(mongodb_branch_names)
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
    use crate::{client::connect_params::ConnectParams, crd::session::SessionCiInfo};

    /// A test case for the [`target_connect_url`] test.
    ///
    /// We have a lot of parameters for that case, and without this struct it's hard to
    /// see which parameter is which.
    #[derive(Debug)]
    struct TargetConnectUrlTestCase {
        use_proxy: bool,
        target: ResolvedTarget<true>,
        concurrent_steal: ConcurrentSteal,
        profile: Option<&'static str>,
        kafka_splits: HashMap<&'static str, BTreeMap<String, String>>,
        sqs_splits: HashMap<&'static str, BTreeMap<String, String>>,
        mysql_branch_names: Vec<String>,
        pg_branch_names: Vec<String>,
        mongodb_branch_names: Vec<String>,
        session_ci_info: Option<SessionCiInfo>,
        key: Option<&'static str>,
        expected: &'static str,
    }

    impl Default for TargetConnectUrlTestCase {
        fn default() -> Self {
            Self {
                use_proxy: Default::default(),
                target: ResolvedTarget::Deployment(ResolvedResource {
                    resource: Deployment {
                        metadata: ObjectMeta {
                            name: Some("py-serv-deployment".into()),
                            namespace: Some("default".into()),
                            ..Default::default()
                        },
                        spec: None,
                        status: None,
                    }
                    .into(),
                    container: None,
                }),
                concurrent_steal: Default::default(),
                profile: None,
                kafka_splits: Default::default(),
                sqs_splits: Default::default(),
                mysql_branch_names: Default::default(),
                pg_branch_names: Default::default(),
                mongodb_branch_names: Default::default(),
                session_ci_info: Default::default(),
                key: Default::default(),
                expected: Default::default(),
            }
        }
    }

    fn deployment_with_container() -> ResolvedTarget<true> {
        ResolvedTarget::Deployment(ResolvedResource {
            resource: Deployment {
                metadata: ObjectMeta {
                    name: Some("py-serv-deployment".into()),
                    namespace: Some("default".into()),
                    ..Default::default()
                },
                spec: None,
                status: None,
            }
            .into(),
            container: Some("py-serv".into()),
        })
    }

    /// Verifies that [`OperatorApi::target_connect_url`] produces expected URLs.
    ///
    /// These URLs should not change for backward compatibility.
    #[rstest]
    #[case::deployment_no_container_no_proxy(
        TargetConnectUrlTestCase{
            expected: "/apis/operator.metalbear.co/v1/namespaces/default/targets/deployment.py-serv-deployment?connect=true&on_concurrent_steal=abort",
            ..Default::default()
        }
    )]
    #[case::deployment_no_container_proxy(
        TargetConnectUrlTestCase{
            use_proxy: true,
            key: Some("my-key"),
            expected: "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment?connect=true&on_concurrent_steal=abort&key=my-key",
            ..Default::default()
        }
    )]
    #[case::deployment_container_no_proxy(
        TargetConnectUrlTestCase{
            target: deployment_with_container(),
            expected: "/apis/operator.metalbear.co/v1/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv?connect=true&on_concurrent_steal=abort",
            ..Default::default()
            }
    )]
    #[case::deployment_container_proxy_key(
        TargetConnectUrlTestCase{
            use_proxy: true,
            target: deployment_with_container(),
            key: Some("auth-token-123"),
            expected: "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv?connect=true&on_concurrent_steal=abort&key=auth-token-123",
            ..Default::default()
        }
    )]
    #[case::deployment_container_proxy_profile(
        TargetConnectUrlTestCase{
            use_proxy: true,
            target: deployment_with_container(),
            profile: Some("no-steal"),
            expected: "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv?connect=true&on_concurrent_steal=abort&profile=no-steal",
            ..Default::default()
        }
    )]
    #[case::deployment_container_proxy_profile_escape(
        TargetConnectUrlTestCase{
            use_proxy: true,
            target: deployment_with_container(),
            profile: Some("/should?be&escaped"),
            key: Some("key-value"),
            expected: "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv?connect=true&on_concurrent_steal=abort&profile=%2Fshould%3Fbe%26escaped&key=key-value",
            ..Default::default()
        }
    )]
    #[case::deployment_container_proxy_kafka_splits(
        TargetConnectUrlTestCase{
            use_proxy: true,
            target: deployment_with_container(),
            kafka_splits: HashMap::from([(
                "topic-id",
                BTreeMap::from([
                    ("header-1".to_string(), "filter-1".to_string()),
                    ("header-2".to_string(), "filter-2".to_string()),
                ]),
            )]),
            expected: "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv\
        ?connect=true&on_concurrent_steal=abort&kafka_splits=%7B%22topic-id%22%3A%7B%22header-1%22%3A%22filter-1%22%2C%22header-2%22%3A%22filter-2%22%7D%7D",
            ..Default::default()
        }
    )]
    #[case::deployment_container_proxy_sqs_splits(
        TargetConnectUrlTestCase{
            use_proxy: true,
            target: deployment_with_container(),
            sqs_splits: HashMap::from([(
                "topic-id",
                BTreeMap::from([
                    ("header-1".to_string(), "filter-1".to_string()),
                    ("header-2".to_string(), "filter-2".to_string()),
                ]),
            )]),
            key: Some("sqs-key"),
            expected: "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv\
            ?connect=true&on_concurrent_steal=abort&sqs_splits=%7B%22topic-id%22%3A%7B%22header-1%22%3A%22filter-1%22%2C%22header-2%22%3A%22filter-2%22%7D%7D&key=sqs-key",
            ..Default::default()
        }
    )]
    #[case::deployment_container_proxy_mysql_branches(
        TargetConnectUrlTestCase{
            use_proxy: true,
            target: deployment_with_container(),
            mysql_branch_names: vec!["branch-1".into(), "branch-2".into()],
            expected: "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv\
            ?connect=true&on_concurrent_steal=abort&mysql_branch_names=%5B%22branch-1%22%2C%22branch-2%22%5D",
            ..Default::default()
        }
    )]
    #[case::deployment_container_proxy_pg_branches(
        TargetConnectUrlTestCase{
            use_proxy: true,
            target: deployment_with_container(),
            pg_branch_names: vec!["branch-1".into(), "branch-2".into()],
            key: Some("pg-access-key"),
            expected: "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv\
            ?connect=true&on_concurrent_steal=abort&pg_branch_names=%5B%22branch-1%22%2C%22branch-2%22%5D&key=pg-access-key",
            ..Default::default()
        }
    )]
    #[case::deployment_container_proxy_mongodb_branches(
        TargetConnectUrlTestCase{
            use_proxy: true,
            target: deployment_with_container(),
            mongodb_branch_names: vec!["branch-1".into(), "branch-2".into()],
            expected: "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv\
            ?connect=true&on_concurrent_steal=abort&mongodb_branch_names=%5B%22branch-1%22%2C%22branch-2%22%5D",
            ..Default::default()
        }
    )]
    #[case::deployment_no_container_no_proxy_with_session_ci_info(
        TargetConnectUrlTestCase{
            session_ci_info: Some(SessionCiInfo {
                provider: Some("Krzysztof".into()),
                environment: Some("Kresy".into()),
                pipeline: Some("Wschodnie".into()),
                triggered_by: Some("Kononowicz".into())
            }),
            key: Some("ci-key-123"),
            expected: "/apis/operator.metalbear.co/v1/namespaces/default/targets/deployment.py-serv-deployment?connect=true&on_concurrent_steal=abort&session_ci_info=%7B%22provider%22%3A%22Krzysztof%22%2C%22environment%22%3A%22Kresy%22%2C%22pipeline%22%3A%22Wschodnie%22%2C%22triggeredBy%22%3A%22Kononowicz%22%7D&key=ci-key-123",
            ..Default::default()
        }
    )]
    #[case::deployment_with_key_none(
        TargetConnectUrlTestCase{
            use_proxy: true,
            target: deployment_with_container(),
            expected: "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv?connect=true&on_concurrent_steal=abort",
            ..Default::default()
        }
    )]
    #[case::deployment_with_key_special_chars(
        TargetConnectUrlTestCase{
            use_proxy: true,
            target: deployment_with_container(),
            key: Some("key/with?special&chars=value"),
            expected: "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.py-serv-deployment.container.py-serv?connect=true&on_concurrent_steal=abort&key=key%2Fwith%3Fspecial%26chars%3Dvalue",
            ..Default::default()
        }
    )]
    #[test]
    fn target_connect_url(
        #[case] TargetConnectUrlTestCase {
            use_proxy,
            target,
            concurrent_steal,
            profile,
            kafka_splits,
            sqs_splits,
            mysql_branch_names,
            pg_branch_names,
            mongodb_branch_names,
            session_ci_info,
            key,
            expected,
        }: TargetConnectUrlTestCase,
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
            mongodb_branch_names,
            mysql_branch_names,
            pg_branch_names,
            session_ci_info,
            is_default_cluster: None, // Only used in multi-cluster
            sqs_output_queues: Default::default(), // Only used in multi-cluster
            key,
        };

        let produced = OperatorApi::target_connect_url(use_proxy, &target, &params);
        assert_eq!(produced, expected)
    }

    /// Verifies that [`OperatorApi::target_connect_url_from_config`] produces expected URLs.
    ///
    /// These URLs should not change for backward compatibility.
    #[rstest]
    #[case::deployment_no_container_no_proxy(
        false,
        mirrord_config::target::Target::Deployment(mirrord_config::target::deployment::DeploymentTarget {
            deployment: "my-deployment".into(),
            container: None,
        }),
        "my-namespace",
        None,
        "/apis/operator.metalbear.co/v1/namespaces/my-namespace/targets/deployment.my-deployment?connect=true&on_concurrent_steal=abort"
    )]
    #[case::deployment_with_container_no_proxy(
        false,
        mirrord_config::target::Target::Deployment(mirrord_config::target::deployment::DeploymentTarget {
            deployment: "my-deployment".into(),
            container: Some("my-container".into()),
        }),
        "my-namespace",
        Some("auth-key-456"),
        "/apis/operator.metalbear.co/v1/namespaces/my-namespace/targets/deployment.my-deployment.container.my-container?connect=true&on_concurrent_steal=abort&key=auth-key-456"
    )]
    #[case::deployment_with_container_proxy(
        true,
        mirrord_config::target::Target::Deployment(mirrord_config::target::deployment::DeploymentTarget {
            deployment: "my-deployment".into(),
            container: Some("my-container".into()),
        }),
        "my-namespace",
        None,
        "/apis/operator.metalbear.co/v1/proxy/namespaces/my-namespace/targets/deployment.my-deployment.container.my-container?connect=true&on_concurrent_steal=abort"
    )]
    #[case::targetless_no_proxy(
        false,
        mirrord_config::target::Target::Targetless,
        "default",
        Some("targetless-key"),
        "/apis/operator.metalbear.co/v1/namespaces/default/targets/targetless?connect=true&on_concurrent_steal=abort&key=targetless-key"
    )]
    #[case::targetless_proxy(
        true,
        mirrord_config::target::Target::Targetless,
        "default",
        None,
        "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/targetless?connect=true&on_concurrent_steal=abort"
    )]
    #[case::pod_no_proxy(
        false,
        mirrord_config::target::Target::Pod(mirrord_config::target::pod::PodTarget {
            pod: "my-pod".into(),
            container: None,
        }),
        "test-ns",
        Some("pod-access-key"),
        "/apis/operator.metalbear.co/v1/namespaces/test-ns/targets/pod.my-pod?connect=true&on_concurrent_steal=abort&key=pod-access-key"
    )]
    #[case::deployment_with_key_none(
        true,
        mirrord_config::target::Target::Deployment(mirrord_config::target::deployment::DeploymentTarget {
            deployment: "test-deployment".into(),
            container: Some("test-container".into()),
        }),
        "default",
        None,
        "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.test-deployment.container.test-container?connect=true&on_concurrent_steal=abort"
    )]
    #[case::deployment_with_key_special_chars(
        true,
        mirrord_config::target::Target::Deployment(mirrord_config::target::deployment::DeploymentTarget {
            deployment: "test-deployment".into(),
            container: Some("test-container".into()),
        }),
        "default",
        Some("key/with?special&chars=value"),
        "/apis/operator.metalbear.co/v1/proxy/namespaces/default/targets/deployment.test-deployment.container.test-container?connect=true&on_concurrent_steal=abort&key=key%2Fwith%3Fspecial%26chars%3Dvalue"
    )]
    #[test]
    fn target_connect_url_from_config(
        #[case] use_proxy: bool,
        #[case] target: mirrord_config::target::Target,
        #[case] namespace: &str,
        #[case] key: Option<&str>,
        #[case] expected: &str,
    ) {
        let params = ConnectParams {
            connect: true,
            on_concurrent_steal: Some(ConcurrentSteal::Abort),
            profile: None,
            kafka_splits: Default::default(),
            sqs_splits: Default::default(),
            branch_name: None,
            mongodb_branch_names: Default::default(),
            mysql_branch_names: Default::default(),
            pg_branch_names: Default::default(),
            session_ci_info: None,
            is_default_cluster: None,
            sqs_output_queues: Default::default(),
            key,
        };
        let produced =
            OperatorApi::target_connect_url_from_config(use_proxy, &target, namespace, &params);
        assert_eq!(produced, expected)
    }
}
