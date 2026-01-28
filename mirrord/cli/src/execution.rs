#[cfg(target_os = "macos")]
use std::ffi::OsString;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    time::Duration,
};

use mirrord_analytics::{AnalyticsError, AnalyticsReporter, Reporter};
use mirrord_config::{
    LayerConfig, MIRRORD_LAYER_INTPROXY_ADDR, config::ConfigError,
    external_proxy::MIRRORD_EXTPROXY_TLS_SETUP_PEM, feature::env::mapper::EnvVarsRemapper,
};
use mirrord_intproxy::agent_conn::AgentConnectInfo;
use mirrord_progress::Progress;
use mirrord_protocol::{ClientMessage, DaemonMessage, EnvVars, GetEnvVarsRequest, LogLevel};
use mirrord_protocol_io::{Client, Connection};
#[cfg(target_os = "macos")]
use mirrord_sip::{SipError, SipPatchOptions, sip_patch};
use mirrord_tls_util::SecureChannelSetup;
use semver::Version;
use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStderr, Command},
    select,
    sync::mpsc::{self, UnboundedReceiver},
};
use tokio_util::sync::CancellationToken;
use tracing::{Level, debug, error, info, trace, warn};

#[cfg(target_os = "macos")]
use crate::extract::extract_arm64;
#[cfg(unix)]
use crate::util::reparent_to_init;
use crate::{
    CliResult, MirrordCi,
    connection::{AGENT_CONNECT_INFO_ENV_KEY, create_and_connect},
    error::CliError,
    extract::extract_library,
    util::{get_user_git_branch, remove_proxy_env},
};

/// Environment variable for saving the execution kind for analytics.
pub const MIRRORD_EXECUTION_KIND_ENV: &str = "MIRRORD_EXECUTION_KIND";

/// Alias to "LD_PRELOAD" enviromnent variable used to mount mirrord-layer on linux targets and as
/// part of the `mirrord container` command.
pub(crate) const LINUX_INJECTION_ENV_VAR: &str = "LD_PRELOAD";

#[cfg(target_os = "linux")]
pub(crate) const INJECTION_ENV_VAR: &str = LINUX_INJECTION_ENV_VAR;

#[cfg(target_os = "macos")]
pub(crate) const INJECTION_ENV_VAR: &str = "DYLD_INSERT_LIBRARIES";

/// A handle to a running mirrord proxy (either internal proxy or external proxy).
#[derive(Debug, Serialize)]
pub(crate) struct MirrordExecution {
    /// Variables to set in the user application environment.
    pub environment: HashMap<String, String>,

    /// Proxy process.
    #[serde(skip)]
    child: Child,

    /// The path to the patched binary, if patched for SIP sidestepping.
    pub patched_path: Option<String>,

    /// Variables to unset in the user application environment.
    pub env_to_unset: Vec<String>,

    /// Whether this run uses mirrord operator.
    pub uses_operator: bool,
}

/// Struct that when dropped will cancel the token and wait on the join handle
/// then update progress with the warnings returned.
struct DropProgress<'a, P>
where
    P: Progress,
{
    progress: &'a P,
    cancellation_token: CancellationToken,
    stderr_rx: UnboundedReceiver<String>,
}

impl<P> Drop for DropProgress<'_, P>
where
    P: Progress,
{
    fn drop(&mut self) {
        self.cancellation_token.cancel();

        while let Ok(line) = self.stderr_rx.try_recv() {
            self.progress
                .warning(format!("internal proxy stderr: {line}").as_str());
        }
    }
}

/// Creates a task that reads stderr and returns a vector of warnings at the end.
/// Caller should cancel the token and wait on join handle.
async fn watch_stderr<P>(stderr: ChildStderr, progress: &P) -> DropProgress<'_, P>
where
    P: Progress,
{
    let cancellation_token = CancellationToken::new();
    let stderr_reader_token = cancellation_token.clone();

    let (stderr_tx, stderr_rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        let mut stderr = BufReader::new(stderr).lines();

        loop {
            select! {
                _ = stderr_reader_token.cancelled() => {
                    trace!("watch_stderr cancelled");
                    break;
                }

                line = stderr.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            debug!("watch_stderr got line {line:?}",);
                            if stderr_tx.send(line).is_err() {
                                break;
                            }
                        },
                        Ok(None) => {
                            trace!("watch_stderr finished");
                            break;
                        }
                        Err(error) => {
                            trace!("watch_stderr error: {error}");
                            break;
                        }
                    }
                }
            }
        }
    });

    DropProgress {
        cancellation_token,
        stderr_rx,
        progress,
    }
}

impl MirrordExecution {
    /// Makes the agent connection and starts the internal proxy child process.
    ///
    /// # Internal proxy
    ///
    /// The internal proxy will be killed as soon as this struct is dropped.
    /// It **does not** happen when you `exec` into user binary, because Rust destructors are not
    /// run. The whole process is instantly replaced by the OS.
    ///
    /// Therefore, everything should work fine when you create [`MirrordExecution`] with this
    /// function and then either:
    /// 1. Drop this struct when an error occurs,
    /// 2. Successfully `exec`,
    /// 3. Wait for intproxy exit with [`MirrordExecution::wait`].
    ///
    /// # `async` shenanigans when using the mirrord operator.
    ///
    /// Here we connect a websocket to the operator created agent, and this connection
    /// might get reset if we don't drive its IO (call `await` on the runtime after the
    /// websocket is up). This is an issue because we start things with `execv`, so we're
    /// kinda out of the whole Rust world of nicely dropping things.
    ///
    /// tl;dr: In `exec_process`, you need to call and `await` either
    /// [`tokio::time::sleep`] or [`tokio::task::yield_now`] after calling this function.
    ///
    /// # Returns
    ///
    /// Returned [`MirrordExecution::environment`] contains everything that the user application
    /// might need, including [`INJECTION_ENV_VAR`] and [`LayerConfig::RESOLVED_CONFIG_ENV`].
    #[tracing::instrument(level = Level::DEBUG, skip_all, ret, err(level = Level::DEBUG))]
    pub(crate) async fn start_internal<P>(
        config: &mut LayerConfig,
        // We only need the executable and args on macos, for SIP handling.
        #[cfg(target_os = "macos")] executable: Option<&str>,
        #[cfg(target_os = "macos")] args: Option<&[OsString]>,
        progress: &mut P,
        analytics: &mut AnalyticsReporter,
        mirrord_for_ci: Option<&MirrordCi>,
    ) -> CliResult<Self>
    where
        P: Progress,
    {
        // Extract Layer from exe, or use existing file if MIRRORD_LAYER_FILE env var is set (for
        // debugging)
        let lib_path = match std::env::var("MIRRORD_LAYER_FILE") {
            Ok(existing_path) => {
                tracing::debug!(
                    "Using existing library file from MIRRORD_LAYER_FILE: {}",
                    existing_path
                );
                std::path::PathBuf::from(existing_path)
            }
            Err(_) => {
                tracing::debug!("MIRRORD_LAYER_FILE not set, extracting library from binary");
                extract_library(None, progress, true)?
            }
        };

        if !config.use_proxy {
            remove_proxy_env();
        }

        let branch_name = get_user_git_branch().await;

        let (connect_info, mut connection) =
            create_and_connect(config, progress, analytics, branch_name, mirrord_for_ci)
                .await
                .inspect_err(|_| analytics.set_error(AnalyticsError::AgentConnection))?;

        let agent_protocol_version = match &connect_info {
            AgentConnectInfo::Operator(session) => session.operator_protocol_version.clone(),
            AgentConnectInfo::DirectKubernetes(_) => {
                Some(MirrordExecution::get_agent_version(&mut connection).await?)
            }
            _ => None,
        };

        config
            .feature
            .network
            .incoming
            .http_filter
            .ensure_usable_with(agent_protocol_version)?;

        let mut env_vars = if config.feature.env.load_from_process.unwrap_or(false) {
            Default::default()
        } else {
            Self::fetch_env_vars(config, &mut connection)
                .await
                .inspect_err(|_| analytics.set_error(AnalyticsError::EnvFetch))?
        };

        #[cfg(target_os = "macos")]
        {
            env_vars.insert(
                "MIRRORD_MACOS_ARM64_LIBRARY".to_string(),
                extract_arm64(progress, true)?.to_string_lossy().into(),
            );

            // Fixes <https://github.com/metalbear-co/mirrord/issues/1745>
            // by disabling the fork safety check in the Objective-C runtime.
            env_vars.insert(
                "OBJC_DISABLE_INITIALIZE_FORK_SAFETY".to_string(),
                "YES".to_string(),
            );
        }

        let lib_path = lib_path.to_string_lossy().into_owned();
        #[cfg(not(target_os = "windows"))]
        {
            // Set LD_PRELOAD/DYLD_INSERT_LIBRARIES
            // If already exists, we append.
            if let Ok(v) = std::env::var(INJECTION_ENV_VAR) {
                env_vars.insert(INJECTION_ENV_VAR.to_string(), format!("{v}:{lib_path}"))
            } else {
                env_vars.insert(INJECTION_ENV_VAR.to_string(), lib_path)
            };
        }
        #[cfg(target_os = "windows")]
        {
            unsafe { std::env::set_var("MIRRORD_LAYER_FILE", lib_path) };
        }

        let encoded_config = config.encode()?;

        let mut proxy_command =
            Command::new(std::env::current_exe().map_err(CliError::CliPathError)?);
        proxy_command.arg("intproxy");

        if mirrord_for_ci.is_some() {
            proxy_command.arg("--mirrord-for-ci");
        }

        proxy_command
            // Start of debug args. Don't add real args after this point,
            // `_debug_args` Clap field will swallow them.
            .arg("--log-destination")
            .arg(config.internal_proxy.log_destination.as_os_str())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .env(
                AGENT_CONNECT_INFO_ENV_KEY,
                serde_json::to_string(&connect_info)?,
            )
            .env(LayerConfig::RESOLVED_CONFIG_ENV, &encoded_config);

        #[cfg(unix)]
        unsafe {
            proxy_command.pre_exec(|| reparent_to_init().map_err(Into::into));
        }

        let mut proxy_process = proxy_command.spawn().map_err(|e| {
            CliError::InternalProxySpawnError(format!("failed to spawn child process: {e}"))
        })?;

        let stderr = proxy_process.stderr.take().expect("stderr was piped");
        let _stderr_guard = watch_stderr(stderr, progress).await;

        let stdout = proxy_process.stdout.take().expect("stdout was piped");

        // Windows-Compatibility: this wait hangs after agent EnvVarsResponse
        //  Skipping it works around the issue.
        #[cfg(unix)]
        {
            // The pre_exec(reparent_to_init) causes the process to fork and our immediate child
            // promptly exits (which is what we wait for here), reparenting our (now former)
            // grandchild to init.
            // This should *never* fail, see https://man7.org/linux/man-pages/man2/wait.2.html for
            // reference.
            proxy_process.wait().await.unwrap();
        }

        let intproxy_address: SocketAddr = BufReader::new(stdout)
            .lines()
            .next_line()
            .await
            .map_err(|e| {
                CliError::InternalProxySpawnError(format!("failed to read proxy stdout: {e}"))
            })?
            .ok_or_else(|| {
                CliError::InternalProxySpawnError(
                    "proxy did not print port number to stdout".to_string(),
                )
            })?
            .parse()
            .map_err(|e| {
                CliError::InternalProxySpawnError(format!(
                    "failed to parse port number printed by proxy: {e}"
                ))
            })?;

        env_vars.insert(LayerConfig::RESOLVED_CONFIG_ENV.into(), encoded_config);
        env_vars.insert(
            MIRRORD_LAYER_INTPROXY_ADDR.into(),
            intproxy_address.to_string(),
        );

        #[cfg(target_os = "macos")]
        let log_info = config
            .experimental
            .sip_log_destination
            .as_ref()
            .map(|log_destination| mirrord_sip::SipLogInfo {
                log_destination,
                args,
                load_type: None,
            });

        #[cfg(target_os = "macos")]
        let patched_path = executable
            .and_then(|exe| {
                sip_patch(
                    exe,
                    SipPatchOptions {
                        patch: &config
                            .sip_binaries
                            .clone()
                            .map(|x| x.to_vec())
                            .unwrap_or_default(),
                        skip: &config.skip_sip,
                    },
                    log_info,
                )
                .transpose() // We transpose twice to propagate a possible error out of this
                // closure.
            })
            .transpose()
            .inspect_err(|sip_error| {
                // we can't recover from hitting the fd limit, so we have to exit fully
                if let SipError::TooManyFilesOpen(..) = sip_error {
                    panic!("mirrord failed to patch SIP with: {}", sip_error);
                }
            })?;

        #[cfg(not(target_os = "macos"))]
        let patched_path = None;

        Ok(Self {
            environment: env_vars,
            child: proxy_process,
            patched_path,
            env_to_unset: config
                .feature
                .env
                .unset
                .clone()
                .map(|unset| unset.to_vec())
                .unwrap_or_default(),
            uses_operator: matches!(connect_info, AgentConnectInfo::Operator(..)),
        })
    }

    async fn get_agent_version(connection: &mut Connection<Client>) -> CliResult<Version> {
        connection
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await;

        match connection.recv().await {
            Some(DaemonMessage::SwitchProtocolVersionResponse(version)) => Ok(version),
            Some(msg) => Err(CliError::InitialAgentCommFailed(format!(
                "received unexpected message during agent version check: {msg:?}"
            ))),
            None => Err(CliError::InitialAgentCommFailed(
                "no response received from agent connection during agent version check".to_string(),
            )),
        }
    }

    /// Makes the agent connection and starts the external proxy child process.
    ///
    /// The external proxy will be used by the internal proxy to talk to the agent.
    ///
    /// # Returns
    ///
    /// Returns the proxy handle as well as the external proxy address.
    /// The address should be accessible from the internal proxy sidecar.
    ///
    /// Returned [`MirrordExecution::environment`] contains *only* remote environment.
    #[tracing::instrument(level = Level::TRACE, skip_all)]
    pub(crate) async fn start_external<P>(
        config: &mut LayerConfig,
        progress: &mut P,
        analytics: &mut AnalyticsReporter,
        tls: Option<&SecureChannelSetup>,
    ) -> CliResult<(Self, SocketAddr)>
    where
        P: Progress,
    {
        if !config.use_proxy {
            remove_proxy_env();
        }

        let branch_name = get_user_git_branch().await;

        let (connect_info, mut connection) =
            create_and_connect(config, progress, analytics, branch_name, None)
                .await
                .inspect_err(|_| analytics.set_error(AnalyticsError::AgentConnection))?;

        let env_vars = if config.feature.env.load_from_process.unwrap_or(false) {
            Default::default()
        } else {
            Self::fetch_env_vars(config, &mut connection)
                .await
                .inspect_err(|_| analytics.set_error(AnalyticsError::EnvFetch))?
        };

        let encoded_config = config.encode()?;

        let mut proxy_command =
            Command::new(std::env::current_exe().map_err(CliError::CliPathError)?);
        proxy_command
            .arg("extproxy")
            // Start of debug args. Don't add real args after this point,
            // `_debug_args` Clap field will swallow them.
            .arg("--extproxy-log-destination")
            .arg(config.external_proxy.log_destination.as_os_str())
            .arg("--intproxy-log-destination")
            .arg(config.internal_proxy.log_destination.as_os_str())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .env(
                AGENT_CONNECT_INFO_ENV_KEY,
                serde_json::to_string(&connect_info)?,
            )
            .env(LayerConfig::RESOLVED_CONFIG_ENV, &encoded_config);

        if let Some(tls) = tls {
            proxy_command.env(MIRRORD_EXTPROXY_TLS_SETUP_PEM, tls.server_pem());
        }

        let mut proxy_process = proxy_command.spawn().map_err(|e| {
            CliError::InternalProxySpawnError(format!("failed to spawn child process: {e}"))
        })?;

        let stderr = proxy_process.stderr.take().expect("stderr was piped");
        let _stderr_guard = watch_stderr(stderr, progress).await;

        let stdout = proxy_process.stdout.take().expect("stdout was piped");

        let proxy_addr: SocketAddr = BufReader::new(stdout)
            .lines()
            .next_line()
            .await
            .map_err(|e| {
                CliError::InternalProxySpawnError(format!("failed to read proxy stdout: {e}"))
            })?
            .ok_or_else(|| {
                CliError::InternalProxySpawnError(
                    "proxy did not print port number to stdout".to_string(),
                )
            })?
            .parse()
            .map_err(|e| {
                CliError::InternalProxySpawnError(format!(
                    "failed to parse port number printed by proxy: {e}"
                ))
            })?;

        let execution = Self {
            environment: env_vars,
            child: proxy_process,
            patched_path: None,
            env_to_unset: config
                .feature
                .env
                .unset
                .clone()
                .map(|unset| unset.to_vec())
                .unwrap_or_default(),
            uses_operator: matches!(connect_info, AgentConnectInfo::Operator(..)),
        };

        Ok((execution, proxy_addr))
    }

    /// Construct filter and retrieve remote environment from the connected agent using
    /// `MirrordExecution::get_remote_env`.
    async fn fetch_env_vars(
        config: &LayerConfig,
        connection: &mut Connection<Client>,
    ) -> CliResult<HashMap<String, String>> {
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
            (Some(..), Some(..)) => {
                return Err(CliError::ConfigError(ConfigError::Conflict(
                    "cannot use both `include` and `exclude` filters for environment variables"
                        .to_string(),
                )));
            }
            (Some(exclude), None) => (HashSet::from(EnvVars(exclude)), HashSet::new()),
            (None, Some(include)) => (HashSet::new(), HashSet::from(EnvVars(include))),
            (None, None) => (HashSet::new(), HashSet::from(EnvVars("*".to_owned()))),
        };

        let mut env_vars = if !env_vars_exclude.is_empty() || !env_vars_include.is_empty() {
            let communication_timeout =
                Duration::from_secs(config.agent.communication_timeout.unwrap_or(30).into());

            tokio::time::timeout(
                communication_timeout,
                Self::get_remote_env(connection, env_vars_exclude, env_vars_include),
            )
            .await
            .map_err(|_| CliError::InitialAgentCommFailed("timeout".to_string()))??
        } else {
            Default::default()
        };

        if let Some(file) = &config.feature.env.env_file {
            let envs_from_file = dotenvy::from_path_iter(file)
                .and_then(|iter| iter.collect::<Result<Vec<_>, _>>())
                .map_err(|error| CliError::EnvFileAccessError(file.clone(), error))?;

            env_vars.extend(envs_from_file);
        }

        if let Some(mapping) = config.feature.env.mapping.clone() {
            env_vars = EnvVarsRemapper::new(mapping, env_vars)
                .expect("Failed creating regex, this should've been caught when verifying config!")
                .remapped();
        }

        if let Some(overrides) = &config.feature.env.r#override {
            env_vars.extend(overrides.iter().map(|(k, v)| (k.clone(), v.clone())));
        }

        Ok(env_vars)
    }

    /// Retrieve remote environment from the connected agent.
    #[tracing::instrument(level = Level::TRACE, skip_all)]
    async fn get_remote_env(
        connection: &mut Connection<Client>,
        env_vars_filter: HashSet<String>,
        env_vars_select: HashSet<String>,
    ) -> CliResult<HashMap<String, String>> {
        connection
            .send(ClientMessage::GetEnvVarsRequest(GetEnvVarsRequest {
                env_vars_filter,
                env_vars_select,
            }))
            .await;

        loop {
            let result = match connection.recv().await {
                Some(DaemonMessage::GetEnvVarsResponse(Ok(remote_env))) => {
                    tracing::trace!(?remote_env, "Agent responded with the remote env");
                    Ok(remote_env)
                }
                Some(DaemonMessage::GetEnvVarsResponse(Err(error))) => {
                    tracing::error!(?error, "Agent responded with an error");
                    Err(CliError::InitialAgentCommFailed(format!(
                        "agent responded with an error: {error}"
                    )))
                }
                Some(DaemonMessage::LogMessage(msg)) => {
                    match msg.level {
                        LogLevel::Error => error!("Agent log: {}", msg.message),
                        LogLevel::Warn => warn!("Agent log: {}", msg.message),
                        LogLevel::Info => info!("Agent log: {}", msg.message),
                    }

                    continue;
                }
                Some(DaemonMessage::Close(msg)) => Err(CliError::InitialAgentCommFailed(format!(
                    "agent closed connection with message: {msg}"
                ))),
                Some(msg) => Err(CliError::InitialAgentCommFailed(format!(
                    "agent responded with an unexpected message: {msg:?}"
                ))),
                None => Err(CliError::InitialAgentCommFailed(
                    "agent unexpectedly closed connection".to_string(),
                )),
            };

            return result.map(Into::into);
        }
    }

    /// Wait for the internal proxy to exit.
    /// Required when called from extension since sometimes the extension
    /// cleans up the process when the parent process exits, so we need the parent to stay alive
    /// while the internal proxy is running.
    /// See <https://github.com/metalbear-co/mirrord/issues/1211>
    pub(crate) async fn wait(mut self) -> CliResult<()> {
        self.child
            .wait()
            .await
            .map_err(CliError::InternalProxyWaitError)?;

        Ok(())
    }
}
