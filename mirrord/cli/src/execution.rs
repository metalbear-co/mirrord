use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use mirrord_analytics::{AnalyticsError, AnalyticsReporter};
use mirrord_config::LayerConfig;
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage, EnvVars, GetEnvVarsRequest};
#[cfg(target_os = "macos")]
use mirrord_sip::sip_patch;
use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStderr, Command},
    select,
    sync::RwLock,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace};

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

/// Struct that when dropped will cancel the token and wait on the join handle
/// then update progress with the warnings returned.
struct DropProgress<'a, P>
where
    P: Progress + Send + Sync,
{
    progress: &'a P,
    cancellation_token: CancellationToken,
    // option so we can `.take()` it in Drop
    messages: Arc<RwLock<Vec<String>>>,
}

impl<'a, P> Drop for DropProgress<'a, P>
where
    P: Progress + Send + Sync,
{
    fn drop(&mut self) {
        self.cancellation_token.cancel();
        match self.messages.try_read() {
            Ok(messages) => messages.iter().for_each(|msg| {
                self.progress
                    .warning(&format!("internal proxy stderr: {}", &msg));
            }),
            Err(e) => error!("internal proxy lock stderr: {e}"),
        }
    }
}

/// Creates a task that reads stderr and returns a vector of warnings at the end.
/// Caller should cancel the token and wait on join handle.
async fn watch_stderr<P>(stderr: ChildStderr, progress: &P) -> DropProgress<P>
where
    P: Progress + Send + Sync,
{
    let our_token = CancellationToken::new();
    let caller_token = our_token.clone();
    let messages = Arc::new(RwLock::new(Vec::new()));
    let caller_messages = messages.clone();

    tokio::spawn(async move {
        let mut stderr = BufReader::new(stderr).lines();
        loop {
            select! {
                    _ = our_token.cancelled() => {
                        trace!("watch_stderr cancelled");
                        break;
                    }
                    line = stderr.next_line() => {
                        match line {
                            Ok(Some(line)) => {
                                debug!("watch_stderr got line: {line}",);
                                messages.write().await.push(line.to_string());
                            },
                            Ok(None) => {
                                trace!("watch_stderr finished");
                                break;
                            }
                            Err(e) => {
                                trace!("watch_stderr error: {e}");
                                break;
                            }
                        }
                    }
            }
        }
    });
    DropProgress {
        cancellation_token: caller_token,
        messages: caller_messages,
        progress,
    }
}

impl MirrordExecution {
    pub(crate) async fn start<P>(
        config: &LayerConfig,
        // We only need the executable on macos, for SIP handling.
        #[cfg(target_os = "macos")] executable: Option<&str>,
        progress: &P,
        analytics: &mut AnalyticsReporter,
    ) -> Result<Self>
    where
        P: Progress + Send + Sync,
    {
        let warnings = config.verify()?;
        for warning in warnings {
            progress.warning(&warning);
        }

        let lib_path = extract_library(None, progress, true)?;

        let (connect_info, mut connection) = create_and_connect(config, progress, analytics)
            .await
            .inspect_err(|_| analytics.set_error(AnalyticsError::AgentConnection))?;

        let mut env_vars = Self::fetch_env_vars(config, &mut connection)
            .await
            .inspect_err(|_| analytics.set_error(AnalyticsError::EnvFetch))?;

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
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null());

        let connect_info = serde_json::to_string(&connect_info)?;
        proxy_command.env(AgentConnectInfo::env_key(), connect_info);

        let mut proxy_process = proxy_command
            .spawn()
            .map_err(CliError::InternalProxyExecutionFailed)?;

        let stderr = proxy_process
            .stderr
            .take()
            .ok_or(CliError::InternalProxyStderrError)?;
        let _stderr_guard = watch_stderr(stderr, progress).await;

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

        // Fix https://github.com/metalbear-co/mirrord/issues/1745
        // by disabling the fork safety check in the Objective-C runtime.
        #[cfg(target_os = "macos")]
        env_vars.insert(
            "OBJC_DISABLE_INITIALIZE_FORK_SAFETY".to_string(),
            "YES".to_string(),
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

    /// Construct filter and retrieve remote environment from the connected agent using
    /// `MirrordExecution::get_remote_env`.
    async fn fetch_env_vars(
        config: &LayerConfig,
        connection: &mut AgentConnection,
    ) -> Result<HashMap<String, String>> {
        let mut env_vars = HashMap::new();

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
                Self::get_remote_env(connection, env_vars_exclude, env_vars_include),
            )
            .await
            .map_err(|_| {
                CliError::InitialCommFailed("Timeout waiting for remote environment variables.")
            })??;
            env_vars.extend(remote_env);
            if let Some(overrides) = &config.feature.env.r#override {
                env_vars.extend(overrides.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }

        Ok(env_vars)
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
