use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use mirrord_config::LayerConfig;
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage, EnvVars, GetEnvVarsRequest};
use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};
use tracing::trace;

use crate::{
    connection::{create_and_connect, AgentConnectInfo},
    error::CliError,
    extract::extract_library,
    Result,
};

#[cfg(target_os = "linux")]
const INJECTION_ENV_VAR: &str = "LD_PRELOAD";

#[cfg(target_os = "macos")]
const INJECTION_ENV_VAR: &str = "DYLD_INSERT_LIBRARIES";

/// Struct for holding the execution information
/// What agent to connect to, what environment variables to set
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MirrordExecution {
    pub environment: HashMap<String, String>,
}

impl MirrordExecution {
    pub(crate) async fn start<P>(config: &LayerConfig, progress: &P) -> Result<Self>
    where
        P: Progress + Send + Sync,
    {
        let lib_path = extract_library(None, progress, true)?;
        let mut env_vars = HashMap::new();
        let (connect_info, mut connection) = create_and_connect(config, progress).await?;
        let (env_vars_exclude, env_vars_include) = match (
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

        if !env_vars_exclude.is_empty() || !env_vars_include.is_empty() {
            // TODO: Handle this error. We're just ignoring it here and letting -layer crash later.
            let _codec_result = connection
                .sender
                .send(ClientMessage::GetEnvVarsRequest(GetEnvVarsRequest {
                    env_vars_filter: env_vars_exclude,
                    env_vars_select: env_vars_include,
                }))
                .await;

            match tokio::time::timeout(
                Duration::from_secs(config.agent.communication_timeout.unwrap_or(30).into()),
                connection.receiver.recv(),
            )
            .await
            {
                Ok(Some(DaemonMessage::GetEnvVarsResponse(Ok(remote_env)))) => {
                    trace!("DaemonMessage::GetEnvVarsResponse {:#?}!", remote_env.len());

                    env_vars.extend(remote_env);
                    if let Some(overrides) = &config.feature.env.overrides {
                        env_vars.extend(overrides.iter().map(|(k, v)| (k.clone(), v.clone())));
                    }
                }
                Err(_) => return Err(CliError::GetEnvironmentTimeout),
                Ok(x) => return Err(CliError::InvalidMessage(format!("{x:#?}"))),
            };
        }

        let lib_path: String = lib_path.to_string_lossy().into();
        // Set LD_PRELOAD/DYLD_INSERT_LIBRARIES
        // If already exists, we append.
        if let Ok(v) = std::env::var(INJECTION_ENV_VAR) {
            env_vars.insert(INJECTION_ENV_VAR.to_string(), format!("{v}:{lib_path}"))
        } else {
            env_vars.insert(INJECTION_ENV_VAR.to_string(), lib_path)
        };

        // stderr is inherited so we can see logs/errors.
        let proxy_process = Command::new(std::env::current_exe().map_err(CliError::CliPathError)?)
            .arg("intproxy")
            .stdout(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .spawn()
            .map_err(CliError::InternalProxyExecutionFailed)?;

        let mut stdout = proxy_process
            .stdout
            .ok_or(CliError::InternalProxyStdoutError)?;

        let port: u16 = BufReader::new(stdout)
            .lines()
            .next_line()
            .await
            .map_err(CliError::InternalProxyReadError)?
            .ok_or(CliError::InternalProxyPortReadError)?
            .parse()
            .map_err(CliError::InternalProxyPortParseError)?;

        // Provide details for layer to connect to agent via internal proxy
        env_vars.insert(
            "MIRRORD_CONNECT_TCP".to_string(),
            format!("127.0.0.1:{port}"),
        );

        Ok(Self {
            environment: env_vars,
        })
    }
}
