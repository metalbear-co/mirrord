use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use mirrord_config::LayerConfig;
use mirrord_operator::client::OperatorSessionInformation;
use mirrord_progress::Progress;
use mirrord_protocol::{
    pause::DaemonPauseTarget, ClientMessage, DaemonMessage, EnvVars, GetEnvVarsRequest,
};
#[cfg(target_os = "macos")]
use mirrord_sip::sip_patch;
use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};
use tracing::{info, trace};

use crate::{
    connection::{create_and_connect, AgentConnectInfo, AgentConnection},
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
#[derive(Debug, Serialize)]
pub(crate) struct MirrordExecution {
    pub environment: HashMap<String, String>,

    #[serde(skip)]
    child: Child,

    /// The path to the patched binary, if patched for SIP sidestepping.
    pub patched_path: Option<String>,
}

impl MirrordExecution {
    pub(crate) async fn start<P>(
        config: &LayerConfig,
        // We only need the executable on macos, for SIP handling.
        #[cfg(target_os = "macos")] executable: Option<&str>,
        progress: &P,
        timeout: Option<u64>,
    ) -> Result<Self>
    where
        P: Progress + Send + Sync,
    {
        config.verify()?;
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

        let communication_timeout =
            Duration::from_secs(config.agent.communication_timeout.unwrap_or(30).into());

        if !env_vars_exclude.is_empty() || !env_vars_include.is_empty() {
            let remote_env = tokio::time::timeout(
                communication_timeout,
                Self::get_remote_env(&mut connection, env_vars_exclude, env_vars_include),
            )
            .await
            .map_err(|_| {
                CliError::InitialCommFailed("Timeout waiting for remote environment variables.")
            })??;
            env_vars.extend(remote_env);
            if let Some(overrides) = &config.feature.env.overrides {
                env_vars.extend(overrides.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }

        if config.pause {
            tokio::time::timeout(communication_timeout, Self::request_pause(&mut connection))
                .await
                .map_err(|_| {
                    CliError::InitialCommFailed("Timeout requesting for target container pause.")
                })??;
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
        let mut proxy_command =
            Command::new(std::env::current_exe().map_err(CliError::CliPathError)?);

        // Set timeout when running from extension to be 30 seconds
        // since it might need to compile, build until it runs the actual process
        // and layer connects
        proxy_command
            .arg("intproxy")
            .stdout(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null());

        if let Some(timeout) = timeout {
            proxy_command.arg("-t").arg(timeout.to_string());
        }

        match &connect_info {
            AgentConnectInfo::DirectKubernetes(name, port) => {
                proxy_command.env("MIRRORD_CONNECT_AGENT", name);
                proxy_command.env("MIRRORD_CONNECT_PORT", port.to_string());
            }
            AgentConnectInfo::Operator(session) => {
                proxy_command.env(
                    OperatorSessionInformation::env_key(),
                    serde_json::to_string(&session)?,
                );
            }
        };

        let mut proxy_process = proxy_command
            .spawn()
            .map_err(CliError::InternalProxyExecutionFailed)?;

        let stdout = proxy_process
            .stdout
            .take()
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

        #[cfg(target_os = "macos")]
        let patched_path = executable
            .and_then(|exe| {
                sip_patch(
                    exe,
                    &config
                        .sip_binaries
                        .clone()
                        .map(|x| x.to_vec())
                        .unwrap_or_default(),
                )
                .transpose() // We transpose twice to propagate a possible error out of this
                             // closure.
            })
            .transpose()?;

        #[cfg(not(target_os = "macos"))]
        let patched_path = None;

        Ok(Self {
            environment: env_vars,
            child: proxy_process,
            patched_path,
        })
    }

    /// Retrieve remote environment from the connected agent.
    async fn get_remote_env(
        connection: &mut AgentConnection,
        env_vars_filter: HashSet<String>,
        env_vars_select: HashSet<String>,
    ) -> Result<HashMap<String, String>> {
        connection
            .sender
            .send(ClientMessage::GetEnvVarsRequest(GetEnvVarsRequest {
                env_vars_filter,
                env_vars_select,
            }))
            .await
            .map_err(|_| {
                CliError::InitialCommFailed("Failed to request remote environment variables.")
            })?;

        match connection.receiver.recv().await {
            Some(DaemonMessage::GetEnvVarsResponse(Ok(remote_env))) => {
                trace!("DaemonMessage::GetEnvVarsResponse {:#?}!", remote_env.len());
                Ok(remote_env)
            }
            msg => Err(CliError::InvalidMessage(format!("{msg:#?}"))),
        }
    }

    /// Request target container pause from the connected agent.
    async fn request_pause(connection: &mut AgentConnection) -> Result<()> {
        info!("Requesting target container pause from the agent");
        connection
            .sender
            .send(ClientMessage::PauseTargetRequest(true))
            .await
            .map_err(|_| {
                CliError::InitialCommFailed("Failed to request target container pause.")
            })?;

        match connection.receiver.recv().await {
            Some(DaemonMessage::PauseTarget(DaemonPauseTarget::PauseResponse {
                changed,
                container_paused: true,
            })) => {
                if changed {
                    info!("Target container is now paused.");
                } else {
                    info!("Target container was already paused.");
                }
                Ok(())
            }
            msg => Err(CliError::InvalidMessage(format!("{msg:#?}"))),
        }
    }

    /// Wait for the internal proxy to exit.
    /// Required when called from extension since sometimes the extension
    /// cleans up the process when the parent process exits, so we need the parent to stay alive
    /// while the internal proxy is running.
    /// See https://github.com/metalbear-co/mirrord/issues/1211
    pub(crate) async fn wait(mut self) -> Result<()> {
        self.child
            .wait()
            .await
            .map_err(CliError::InternalProxyWaitError)?;
        Ok(())
    }
}
