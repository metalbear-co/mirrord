use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use mirrord_config::LayerConfig;
use mirrord_kube::api::{kubernetes::KubernetesAPI, AgentManagment};
use mirrord_operator::client::OperatorApiDiscover;
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage, EnvVars, GetEnvVarsRequest};
use tokio::sync::mpsc;
use tracing::log::trace;

use crate::{CliError, Result};

#[cfg(target_os = "linux")]
const INJECTION_ENV_VAR: &str = "LD_PRELOAD";

#[cfg(target_os = "macos")]
const INJECTION_ENV_VAR: &str = "DYLD_INSERT_LIBRARIES";

pub(crate) enum AgentConnectInfo {
    /// No info is needed, operator creates on demand
    Operator,
    /// Connect directly to an agent by name and port using k8s port forward.
    DirectKubernetes(String, u16),
}

struct AgentConnection {
    sender: mpsc::Sender<ClientMessage>,
    receiver: mpsc::Receiver<DaemonMessage>,
}

/// Struct for holding the execution information
/// What agent to connect to, what environment variables to set
pub(crate) struct MirrordExecution {
    pub connect_info: AgentConnectInfo,
    pub environment: HashMap<String, String>,
}

impl MirrordExecution {
    pub(crate) async fn start<P>(config: &LayerConfig, progress: &P) -> Result<Self>
    where
        P: Progress + Send + Sync,
    {
        let config = LayerConfig::from_env()?;
        let mut env_vars = HashMap::new();
        let (connect_info, connection) = create_and_connect(&config, progress).await?;
        let (env_vars_filter, env_vars_select) = match (
            config
                .feature
                .env
                .exclude
                .clone()
                .map(|exclude| exclude.join(";")),
            config
                .feature
                .env
                .include
                .clone()
                .map(|include| include.join(";")),
        ) {
            (Some(exclude), Some(include)) => {
                return Err(CliError::InvalidEnvConfig(include, exclude))
            }
            (Some(exclude), None) => (HashSet::from(EnvVars(exclude)), HashSet::new()),
            (None, Some(include)) => (HashSet::new(), HashSet::from(EnvVars(include))),
            (None, None) => (HashSet::new(), HashSet::from(EnvVars("*".to_owned()))),
        };

        if !env_vars_filter.is_empty() || !env_vars_select.is_empty() {
            // TODO: Handle this error. We're just ignoring it here and letting -layer crash later.
            let _codec_result = connection
                .sender
                .send(ClientMessage::GetEnvVarsRequest(GetEnvVarsRequest {
                    env_vars_filter,
                    env_vars_select,
                }))
                .await;

            match tokio::time::timeout(
                Duration::from_secs(config.agent.communication_timeout.unwrap_or(30).into()),
                connection.receiver.recv(),
            )
            .await
            {
                Ok(Some(DaemonMessage::GetEnvVarsResponse(Ok(mut remote_env)))) => {
                    trace!("DaemonMessage::GetEnvVarsResponse {:#?}!", remote_env.len());
                    if let Some(overrides) = &config.feature.env.overrides {
                        for (key, value) in overrides {
                            remote_env.insert(key.clone(), value.clone());
                        }
                    }
                    env_vars = Some(remote_env)
                }
                Err(_) => return Err(CliError::GetEnvironmentTimeout),
                Ok(x) => return Err(CliError::InvalidMessage("{:?}".to_string())),
            };
        }

        // Set LD_PRELOAD/DYLD_INSERT_LIBRARIES
        // If already exists, we append.
        std::env::var(INJECTION_ENV_VAR)
            .map(|v| {
                env_vars.insert(
                    INJECTION_ENV_VAR.to_string(),
                    format!("{}:{}", value, lib_path).to_string(),
                )
            })
            .map_err(|_| env_vars.insert(INJECTION_ENV_VAR.to_string(), lib_path.to_string()));

        match &connect_info {
            AgentConnectInfo::DirectKubernetes(name, port) => {
                env_vars.insert("MIRRORD_CONNECT_AGENT".to_string(), name.to_string());
                env_vars.insert("MIRRORD_CONNECT_PORT".to_string(), port.to_string());
            }
            AgentConnectInfo::Operator => {}
        };
        
        Ok(Self {
            connect_info,
            environment: env_vars,
        })
    }
}

/// Creates an agent if needed then connects to it.
pub async fn create_and_connect<P>(
    config: &LayerConfig,
    progress: &P,
) -> Result<(AgentConnectInfo, AgentConnection)>
where
    P: Progress + Send + Sync,
{
    if config.operator.enabled && let Some((operator_api, operator_ref)) =
    OperatorApiDiscover::discover_operator(&config, progress).await
    {
    let (sender, receiver) = operator_api
        .create_connection(operator_ref).await.map_err(CliError::OperatorConnectionFailed)?;
    Ok((AgentConnectInfo::Operator, AgentConnection { sender, receiver }))

    } else {
    let k8s_api = KubernetesAPI::create(config)
        .await
        .map_err(CliError::KubernetesApiFailed)?;
    let (pod_agent_name, agent_port) = tokio::time::timeout(
        Duration::from_secs(config.agent.startup_timeout),
        k8s_api.create_agent(&progress),
    )
    .await
    .unwrap_or(Err(CliError::AgentReadyTimeout))
    .map_err(CliError::AgentReadyFailed)?;

    let (sender, receiver) = k8s_api
    .create_connection((pod_agent_name, agent_port))
    .await.map_err(CliError::AgentConnectionFailed)?;

    Ok((AgentConnectInfo::DirectKubernetes(pod_agent_name, agent_port), AgentConnection { sender, receiver }))
    }
}
