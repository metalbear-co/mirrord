use std::{fmt, io, net::SocketAddr, ops::Not, path::PathBuf, process::Stdio, time::Duration};

use futures::{FutureExt, Stream};
use mirrord_analytics::ExecutionKind;
use mirrord_config::{
    config::ConfigError, internal_proxy::MIRRORD_INTPROXY_CONTAINER_MODE_ENV, LayerConfig,
};
use mirrord_intproxy::agent_conn::AgentConnectInfo;
use mirrord_progress::MIRRORD_PROGRESS_ENV;
use mirrord_tls_util::SecureChannelSetup;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, BufReader, Lines},
    process::{ChildStderr, ChildStdout, Command},
};
use tokio_stream::{wrappers::LinesStream, StreamExt};
use tracing::Level;

use super::command_display::CommandDisplay;
use crate::{
    config::ContainerRuntimeCommand,
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    container::{command_builder::RuntimeCommandBuilder, command_display::CommandExt},
    error::ContainerError,
    execution::MIRRORD_EXECUTION_KIND_ENV,
    util::MIRRORD_CONSOLE_ADDR_ENV,
    CliError, ContainerRuntime,
};

/// Errors that can occure when creating or starting the internal proxy sidecar container.
#[derive(Error, Debug)]
pub enum IntproxySidecarError {
    #[error(transparent)]
    EncodeConfigError(#[from] ConfigError),
    #[error("failed to serialize connect info: {0}")]
    SerializeConnectInfoError(#[source] serde_json::Error),
    #[error("failed to execute command [{command}]: {error}")]
    CommandExecuteError {
        command: CommandDisplay,
        #[source]
        error: io::Error,
    },
    #[error("command [{0}] timed out")]
    CommandTimedOut(CommandDisplay),
    #[error("command [{command}] failed: {message}")]
    CommandFailed {
        command: CommandDisplay,
        message: String,
    },
    #[error("failed to read internal proxy address: {0}")]
    FailedToReadIntproxyAddr(
        /// Error message.
        String,
    ),
    #[error("failed to process a non UTF-8 path: {0}")]
    NonUtf8Path(
        /// The original path as lossy UTF-8.
        String,
    ),
}

impl From<IntproxySidecarError> for CliError {
    fn from(value: IntproxySidecarError) -> Self {
        Self::ContainerError(ContainerError::IntproxySidecar(value))
    }
}

/// A sidecar container where the internal proxy runs.
///
/// The container is first created with [`IntproxySidecar::create`].
/// Then, it can be started [`IntproxySidecar::start`].
///
/// # Why a sidecar?
///
/// The internal proxy needs to be accessible from the user application,
/// which will run in its own container.
///
/// Using the `--network` runtime command arg and [`IntproxySidecar::container_id`],
/// we can expose the proxy to that container.
///
/// # Image
///
/// This sidecar container runs the image specified in
/// [`ContainerConfig::cli_image`](mirrord_config::container::ContainerConfig::cli_image),
/// which contains the mirrord binary and extracted mirrord-layer.
///
/// Note that the mirrord-layer inside this image should sit inside a volume.
/// This way you can use the `--volumes--from` runtime command arg and
/// [`IntproxySidecar::container_id`] to add the mirrord-layer file to the user container.
#[derive(Debug)]
pub struct IntproxySidecar {
    container_id: String,
    runtime_binary: String,
}

impl IntproxySidecar {
    /// Creates a sidecar container that will run `mirrord intproxy`.
    ///
    /// This function does not start the container, it only creates it.
    /// You can start the container with [`IntproxySidecar::start`].
    ///
    /// # Environment
    ///
    /// All internal proxy configuration is passed to the sidecar
    /// with environment variables, including:
    /// 1. Fully resolved [`LayerConfig`] ([`LayerConfig::RESOLVED_CONFIG_ENV`]).
    /// 2. Extproxy connect info ([`AGENT_CONNECT_INFO_ENV_KEY`])
    #[tracing::instrument(
        level = Level::DEBUG,
        skip(config, tls), fields(uses_tls = tls.is_some()),
        ret, err(level = Level::DEBUG),
    )]
    pub async fn create(
        config: &LayerConfig,
        container_runtime: ContainerRuntime,
        extproxy_addr: SocketAddr,
        tls: Option<&SecureChannelSetup>,
    ) -> Result<Self, IntproxySidecarError> {
        let mut sidecar_command = RuntimeCommandBuilder::new(container_runtime);

        sidecar_command.add_env(LayerConfig::RESOLVED_CONFIG_ENV, &config.encode()?);

        if let Some(console_addr) = super::get_mirrord_console_addr() {
            sidecar_command.add_env(MIRRORD_CONSOLE_ADDR_ENV, &console_addr);
        }
        sidecar_command.add_env(MIRRORD_PROGRESS_ENV, "off");
        sidecar_command.add_env(
            MIRRORD_EXECUTION_KIND_ENV,
            &(ExecutionKind::Container as u32).to_string(),
        );
        sidecar_command.add_env(MIRRORD_INTPROXY_CONTAINER_MODE_ENV, "true");

        let connect_info = if let Some(tls) = tls {
            let client_pem_path = tls.client_pem().to_str().ok_or_else(|| {
                IntproxySidecarError::NonUtf8Path(tls.client_pem().to_string_lossy().into_owned())
            })?;
            let container_path = "/tmp/mirrord-tls.pem";
            sidecar_command.add_volume(client_pem_path, container_path, true);
            AgentConnectInfo::ExternalProxy {
                proxy_addr: extproxy_addr,
                tls_pem: Some(PathBuf::from(container_path)),
            }
        } else {
            AgentConnectInfo::ExternalProxy {
                proxy_addr: extproxy_addr,
                tls_pem: None,
            }
        };
        sidecar_command.add_env(
            AGENT_CONNECT_INFO_ENV_KEY,
            &serde_json::to_string(&connect_info)
                .map_err(IntproxySidecarError::SerializeConnectInfoError)?,
        );

        let cleanup = config.container.cli_prevent_cleanup.not().then_some("--rm");

        let sidecar_container_command = ContainerRuntimeCommand::create(
            config
                .container
                .cli_extra_args
                .iter()
                .map(String::as_str)
                .chain(cleanup)
                .chain([&config.container.cli_image, "mirrord", "intproxy"]),
        );

        let (runtime_binary, sidecar_args) = sidecar_command
            .with_command(sidecar_container_command)
            .into_command_args();

        let mut sidecar_container_spawn = Command::new(&runtime_binary);
        sidecar_container_spawn.args(sidecar_args);
        let container_id = exec_and_get_first_line(sidecar_container_spawn).await?;

        Ok(IntproxySidecar {
            container_id,
            runtime_binary,
        })
    }

    /// Returns the id of the created container.
    ///
    /// You can use it to reference its volumes or network.
    pub fn container_id(&self) -> &str {
        &self.container_id
    }

    /// Starts the internal proxy sidecar container.
    ///
    /// Returns:
    ///
    /// 1. The address of the internal proxy
    /// 2. Internal proxy's standard streams
    #[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))]
    pub async fn start(self) -> Result<(SocketAddr, SidecarLogs), IntproxySidecarError> {
        let mut command = Command::new(&self.runtime_binary);
        command.args(["start", "--attach", &self.container_id]);

        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| IntproxySidecarError::CommandExecuteError {
                command: command.display(),
                error,
            })?;

        let mut stdout = BufReader::new(child.stdout.take().expect("was piped")).lines();
        let mut stderr = BufReader::new(child.stderr.take().expect("was piped")).lines();

        let first_line = tokio::time::timeout(Duration::from_secs(30), stdout.next_line()).await;
        let intproxy_addr = match first_line {
            Err(..) => {
                let stderr = Self::read_ready_lines(&mut stderr);
                return Err(IntproxySidecarError::FailedToReadIntproxyAddr(format!(
                    "timed out waiting for the first line of stdout, stderr: `{stderr}`",
                )));
            }
            Ok(Err(error)) => {
                let stderr = Self::read_ready_lines(&mut stderr);
                return Err(IntproxySidecarError::FailedToReadIntproxyAddr(format!(
                    "failed to read stdout with {error}, stderr: `{stderr}`",
                )));
            }
            Ok(Ok(None)) => {
                let stderr = Self::read_ready_lines(&mut stderr);
                return Err(IntproxySidecarError::FailedToReadIntproxyAddr(format!(
                    "unexpected EOF when reading stdout, stderr: `{stderr}`",
                )));
            }
            Ok(Ok(Some(line))) => line.parse::<SocketAddr>().map_err(|error| {
                IntproxySidecarError::FailedToReadIntproxyAddr(format!(
                    "failed to parse the address from the first stdout line `{line}`: {error}"
                ))
            })?,
        };

        Ok((intproxy_addr, SidecarLogs { stdout, stderr }))
    }

    /// Reads all ready lines from the given reader.
    ///
    /// Returns the lines concatenated with `\n` chars (to indicate line breaks).
    fn read_ready_lines(stderr: &mut Lines<BufReader<ChildStderr>>) -> String {
        let mut buf = vec![];

        while let Some(result) = stderr.next_line().now_or_never() {
            match result {
                Err(..) => break,
                Ok(None) => break,
                Ok(Some(line)) => buf.push(line),
            }
        }

        buf.join("\\n")
    }
}

/// Live logs from a started [`IntproxySidecar`].
pub struct SidecarLogs {
    stdout: Lines<BufReader<ChildStdout>>,
    stderr: Lines<BufReader<ChildStderr>>,
}

impl SidecarLogs {
    /// Returns the logs as a [`Stream`] of lines,
    /// from both stdout and stderr.
    pub fn into_merged_lines(self) -> impl 'static + Stream<Item = io::Result<String>> {
        LinesStream::new(self.stdout).merge(LinesStream::new(self.stderr))
    }
}

impl fmt::Debug for SidecarLogs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SidecarLogs").finish()
    }
}

/// Executes the given [`Command`] to completion and reads the first line of its standard output.
///
/// Ensures that the first line of output is not empty.
///
/// Respects a 30 second timeout when waiting the command's output.
#[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))]
async fn exec_and_get_first_line(mut command: Command) -> Result<String, IntproxySidecarError> {
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await;

    let output = match result {
        Err(..) => return Err(IntproxySidecarError::CommandTimedOut(command.display())),
        Ok(Err(error)) => {
            return Err(IntproxySidecarError::CommandExecuteError {
                command: command.display(),
                error,
            })
        }
        Ok(Ok(output)) => output,
    };

    if output.status.success().not() {
        return Err(IntproxySidecarError::CommandFailed {
            command: command.display(),
            message: format!(
                "{}, stderr: `{}`",
                output.status,
                String::from_utf8_lossy(&output.stderr),
            ),
        });
    }

    match output.stdout.lines().next_line().await {
        Ok(Some(line)) if line.is_empty().not() => Ok(line),
        Ok(..) => Err(IntproxySidecarError::CommandFailed {
            command: command.display(),
            message: format!(
                "stdout was empty, stderr: `{}`",
                String::from_utf8_lossy(&output.stderr)
            ),
        }),
        Err(error) => Err(IntproxySidecarError::CommandFailed {
            command: command.display(),
            message: format!("failed to read stdout: {error}"),
        }),
    }
}
