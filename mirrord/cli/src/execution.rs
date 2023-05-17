use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use mirrord_config::LayerConfig;
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage, EnvVars, GetEnvVarsRequest};
#[cfg(target_os = "macos")]
use mirrord_sip::sip_patch;
use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};
use tracing::trace;

use crate::{
    connection::{create_and_connect, AgentConnectInfo},
    error::{CliError, CliError::IncompatibleWithTargetless},
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
        Self::check_config_for_targetless_agent(config)?;
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
            AgentConnectInfo::Operator => {}
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

    /// Some features are incompatible with targetless agents, return error if they are set and
    /// there is no target.
    ///
    /// # Errors
    ///
    /// [`IncompatibleWithTargetless`]
    fn check_config_for_targetless_agent(config: &LayerConfig) -> Result<()> {
        if config.target.is_none() {
            if config.feature.network.incoming.is_steal() {
                Err(IncompatibleWithTargetless("Steal mode".into()))?
            }
            if config.agent.ephemeral {
                Err(IncompatibleWithTargetless(
                    "Using an ephemeral container for the agent".into(),
                ))?
            }
            if config.agent.pause {
                Err(IncompatibleWithTargetless(
                    "The target pause feature".into(),
                ))?
            }
        }
        Ok(())
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

// if a proxy process has been created and there is an error, we need to terminate it
impl Drop for MirrordExecution {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}
