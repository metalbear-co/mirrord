use std::{collections::HashSet, error::Error, time::Duration};

use kube::{Client, Config};
use mirrord_analytics::Reporter;
use mirrord_config::LayerConfig;
use mirrord_intproxy::agent_conn::AgentConnectInfo;
use mirrord_kube::{
    api::{
        kubernetes::{create_kube_config, KubernetesAPI},
        wrap_raw_connection,
    },
    error::KubeApiError,
};
use mirrord_operator::{
    client::{
        operator_config, NoClientCert, OperatorApi, OperatorSessionConnection, PreparedClientCert,
    },
    types::{CLIENT_HOSTNAME_HEADER, CLIENT_NAME_HEADER, MIRRORD_CLI_VERSION_HEADER},
};
use mirrord_progress::{
    messages::MULTIPOD_WARNING, IdeAction, IdeMessage, NotificationLevel, Progress,
};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use reqwest::header::{HeaderName, HeaderValue};
use tokio::sync::mpsc;
use tracing::{instrument::WithSubscriber, Level};

use crate::{CliError, Result};

mod operator;

pub const AGENT_CONNECT_INFO_ENV_KEY: &str = "MIRRORD_AGENT_CONNECT_INFO";

pub(crate) struct AgentConnection {
    pub sender: mpsc::Sender<ClientMessage>,
    pub receiver: mpsc::Receiver<DaemonMessage>,
}

pub(super) struct TargetConnection<'a, P, R>
where
    P: Progress,
    R: Reporter,
{
    config: &'a LayerConfig,
    progress: &'a P,
    analytics: &'a mut R,
    kube_config: Config,
    base_kube_client: Client,
    operator: Option<OperatorApi<PreparedClientCert>>,
}

impl<'a, P, R> TargetConnection<'a, P, R>
where
    P: Progress,
    R: Reporter,
{
    async fn new(
        config: &'a LayerConfig,
        progress: &'a P,
        analytics: &'a mut R,
    ) -> Result<Self, Box<dyn Error>> {
        let kube_config = create_kube_config(
            config.accept_invalid_certificates,
            config.kubeconfig.clone(),
            config.kube_context.clone(),
        )
        .await?;

        let base_kube_client = Client::try_from(kube_config.clone())?;

        Ok(Self {
            config,
            progress,
            analytics,
            kube_config,
            base_kube_client,
            operator: None,
        })
    }

    async fn create_kube_client(self) -> Result<Self, Box<dyn Error>> {
        let mut operator_subtask = self.progress.subtask("checking operator");
        if self.config.operator == Some(false) {
            operator_subtask.success(Some("operator disabled"));
            Ok(self)
        } else {
            let operator_config = operator_config(&self.kube_config);
            let operator_kube_client = Client::try_from(operator_config.clone())?;

            let operator_api =
                OperatorApi::try_new_new(&self.kube_config, self.analytics, &operator_kube_client)
                    .await?;

            match operator_api {
                Some(operator_api) => {
                    let mut version_cmp_subtask =
                        operator_subtask.subtask("checking version compatibility");
                    let compatible = operator_api.check_operator_version(&version_cmp_subtask);
                    if compatible {
                        version_cmp_subtask.success(Some("operator version compatible"));
                    } else {
                        version_cmp_subtask.failure(Some("operator version may not be compatible"));
                    }

                    // TODO(alex) [high] 5: Do we have to keep the operator config alive? Nah
                    self.validated_operator_license(&operator_api, &mut operator_subtask)?
                        .certified_operator(operator_api, &mut operator_subtask)
                        .await
                    // TODO(alex) [high] 6: Now that we have `kube_client` as being the operator
                    // client, we can move the operator specific checks to mirrord, and if
                    // `operator: None`, we can just skip these checks, and do line 285 and forwards
                    // from `create_and_connect` .
                }
                None if self.config.operator == Some(true) => {
                    // TODO(alex) [mid] 4: This is an error, we don't proceed!
                    todo!()
                }
                None => {
                    operator_subtask.success(Some("operator not found"));
                    Ok(self)
                }
            }
        }
    }

    fn validated_operator_license(
        self,
        operator_api: &OperatorApi<NoClientCert>,
        operator_subtask: &mut P,
    ) -> Result<Self, Box<dyn Error>> {
        let mut license_subtask = operator_subtask.subtask("checking license");
        match operator_api.check_license_validity(&license_subtask) {
            Ok(()) => {
                license_subtask.success(Some("operator license valid"));
                Ok(self)
            }
            Err(fail) => {
                license_subtask.failure(Some("operator license expired"));

                if self.config.operator == Some(true) {
                    Err(fail.into())
                } else {
                    operator_subtask.failure(Some("proceeding without operator"));
                    Ok(Self {
                        operator: None,
                        ..self
                    })
                }
            }
        }
    }

    async fn certified_operator(
        self,
        operator_api: OperatorApi<NoClientCert>,
        operator_subtask: &mut P,
    ) -> Result<Self, Box<dyn Error>> {
        let mut user_cert_subtask = operator_subtask.subtask("preparing user credentials");
        let prepared_api = operator_api
            .prepare_client_cert(self.analytics)
            .await
            .into_certified()?;
        user_cert_subtask.success(Some("user credentials prepared"));

        let kube_client = prepared_api.client().clone();
        Ok(Self {
            operator: Some(prepared_api),
            base_kube_client: kube_client,
            ..self
        })
    }
}

/// 1. If mirrord-operator is explicitly enabled in the given [`LayerConfig`], makes a connection
///    with the target using the mirrord-operator.
/// 2. If mirrord-operator is explicitly disabled in the given [`LayerConfig`], returns [`None`].
/// 3. Otherwise, attempts to use the mirrord-operator and returns [`None`] in case mirrord-operator
///    is not found or its license is invalid.
async fn try_connect_using_operator<P, R>(
    config: &LayerConfig,
    progress: &P,
    analytics: &mut R,
) -> Result<Option<OperatorSessionConnection>>
where
    P: Progress,
    R: Reporter,
{
    let mut operator_subtask = progress.subtask("checking operator");
    if config.operator == Some(false) {
        operator_subtask.success(Some("operator disabled"));
        return Ok(None);
    }

    let api = match OperatorApi::try_new(config, analytics).await? {
        Some(api) => api,
        None if config.operator == Some(true) => return Err(CliError::OperatorNotInstalled),
        None => {
            operator_subtask.success(Some("operator not found"));
            return Ok(None);
        }
    };

    let mut version_cmp_subtask = operator_subtask.subtask("checking version compatibility");
    let compatible = api.check_operator_version(&version_cmp_subtask);
    if compatible {
        version_cmp_subtask.success(Some("operator version compatible"));
    } else {
        version_cmp_subtask.failure(Some("operator version may not be compatible"));
    }

    let mut license_subtask = operator_subtask.subtask("checking license");
    match api.check_license_validity(&license_subtask) {
        Ok(()) => license_subtask.success(Some("operator license valid")),
        Err(error) => {
            license_subtask.failure(Some("operator license expired"));

            if config.operator == Some(true) {
                return Err(error.into());
            } else {
                operator_subtask.failure(Some("proceeding without operator"));
                return Ok(None);
            }
        }
    }

    let mut user_cert_subtask = operator_subtask.subtask("preparing user credentials");
    let api = api.prepare_client_cert(analytics).await.into_certified()?;
    user_cert_subtask.success(Some("user credentials prepared"));

    let mut session_subtask = operator_subtask.subtask("starting session");
    let connection = api.connect_in_new_session(config, &session_subtask).await?;
    session_subtask.success(Some("session started"));

    operator_subtask.success(Some("using operator"));

    Ok(Some(connection))
}

/// 1. If mirrord-operator is explicitly enabled in the given [`LayerConfig`], makes a connection
///    with the target using the mirrord-operator.
/// 2. If mirrord-operator is explicitly disabled in the given [`LayerConfig`], creates a
///    mirrord-agent and runs session without the mirrord-operator.
/// 3. Otherwise, attempts to use the mirrord-operator and falls back to OSS flow in case
///    mirrord-operator is not found or its license is invalid.
///
/// Here is where we start interactions with the kubernetes API.
pub(crate) async fn create_and_connect<P, R: Reporter>(
    config: &LayerConfig,
    progress: &mut P,
    analytics: &mut R,
) -> Result<(AgentConnectInfo, AgentConnection)>
where
    P: Progress + Send + Sync,
{
    let newstuff = TargetConnection::new(config, progress, analytics)
        .await
        .unwrap()
        .create_kube_client();

    todo!()
}

// #[tracing::instrument(level = Level::TRACE, skip_all)]
pub(crate) async fn create_and_connect2<P, R: Reporter>(
    config: &LayerConfig,
    progress: &mut P,
    analytics: &mut R,
) -> Result<(AgentConnectInfo, AgentConnection)>
where
    P: Progress + Send + Sync,
{
    // TODO(alex) [high] 3: If this is `Ok(None)`, then the config we use is not the one
    // from the operator, it's just the normal user kube config, which will be created after
    // this whole thing.
    if let Some(connection) = try_connect_using_operator(config, progress, analytics).await? {
        return Ok((
            AgentConnectInfo::Operator(connection.session),
            AgentConnection {
                sender: connection.tx,
                receiver: connection.rx,
            },
        ));
    }

    if config.feature.copy_target.enabled {
        return Err(CliError::FeatureRequiresOperatorError("copy_target".into()));
    }

    if matches!(
        config.target,
        mirrord_config::target::TargetConfig {
            path: Some(
                mirrord_config::target::Target::Deployment { .. }
                    | mirrord_config::target::Target::Rollout(..)
            ),
            ..
        }
    ) {
        // Send to IDEs that we're in multi-pod without operator.
        progress.ide(serde_json::to_value(IdeMessage {
            id: MULTIPOD_WARNING.0.to_string(),
            level: NotificationLevel::Warning,
            text: MULTIPOD_WARNING.1.to_string(),
            actions: {
                let mut actions = HashSet::new();
                actions.insert(IdeAction::Link {
                    label: "Get started (read the docs)".to_string(),
                    link: "https://mirrord.dev/docs/overview/teams/?utm_source=multipodwarn&utm_medium=plugin".to_string(),
                });
                actions.insert(IdeAction::Link {
                    label: "Try it now".to_string(),
                    link: "https://app.metalbear.co/".to_string(),
                });

                actions
            },
        })?);
        // This is CLI Only because the extensions also implement this check with better messaging.
        progress.print("When targeting multi-pod deployments, mirrord impersonates the first pod in the deployment.");
        progress.print("Support for multi-pod impersonation requires the mirrord operator, which is part of mirrord for Teams.");
        progress.print("You can get started with mirrord for Teams at this link: https://mirrord.dev/docs/overview/teams/?utm_source=multipodwarn&utm_medium=cli");
    }

    let k8s_api = KubernetesAPI::create(config)
        .await
        .map_err(|error| CliError::auth_exec_error_or(error, CliError::CreateAgentFailed))?;

    if let Err(error) = k8s_api.detect_openshift(progress).await {
        tracing::debug!(?error, "Failed to detect OpenShift");
    };

    let agent_connect_info = tokio::time::timeout(
        Duration::from_secs(config.agent.startup_timeout),
        k8s_api.create_agent(progress, &config.target, Some(config), Default::default()),
    )
    .await
    .unwrap_or(Err(KubeApiError::AgentReadyTimeout))
    .map_err(|error| CliError::auth_exec_error_or(error, CliError::CreateAgentFailed))?;

    let (sender, receiver) = wrap_raw_connection(
        k8s_api
            .create_connection(agent_connect_info.clone())
            .await
            .map_err(|error| {
                CliError::auth_exec_error_or(error, CliError::AgentConnectionFailed)
            })?,
    );

    Ok((
        AgentConnectInfo::DirectKubernetes(agent_connect_info),
        AgentConnection { sender, receiver },
    ))
}
