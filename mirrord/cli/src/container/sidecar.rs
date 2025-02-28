use std::{net::SocketAddr, ops::Not, process::Stdio, time::Duration};

use mirrord_config::{internal_proxy::MIRRORD_INTPROXY_CONTAINER_MODE_ENV, LayerConfig};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{ChildStderr, ChildStdout, Command},
};
use tokio_stream::{wrappers::LinesStream, StreamExt};
use tracing::Level;

use crate::{
    config::ContainerRuntimeCommand,
    container::{command_builder::RuntimeCommandBuilder, exec_and_get_first_line, format_command},
    error::ContainerError,
};

#[derive(Debug)]
pub(crate) struct Sidecar {
    pub container_id: String,
    pub runtime_binary: String,
}

impl Sidecar {
    /// Create a "sidecar" container that is running `mirrord intproxy` that connects to `mirrord
    /// extproxy` running on user machine to be used by execution container (via mounting on same
    /// network)
    #[tracing::instrument(level = Level::DEBUG, skip(config), ret, err)]
    pub async fn create_intproxy(
        config: &LayerConfig,
        base_command: &RuntimeCommandBuilder,
        connection_info: Vec<(&str, &str)>,
    ) -> Result<Self, ContainerError> {
        let mut sidecar_command = base_command.clone();

        sidecar_command.add_env(MIRRORD_INTPROXY_CONTAINER_MODE_ENV, "true");
        sidecar_command.add_envs(connection_info);

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

        let container_id = exec_and_get_first_line(&mut sidecar_container_spawn)
            .await?
            .ok_or_else(|| {
                ContainerError::UnsuccesfulCommandOutput(
                    format_command(&sidecar_container_spawn),
                    "stdout and stderr were empty".to_owned(),
                )
            })?;

        Ok(Sidecar {
            container_id,
            runtime_binary,
        })
    }

    pub fn as_network(&self) -> String {
        let Sidecar { container_id, .. } = self;
        format!("container:{container_id}")
    }

    #[tracing::instrument(level = Level::DEBUG, err)]
    pub async fn start(&self) -> Result<(SocketAddr, SidecarLogs), ContainerError> {
        let mut command = Command::new(&self.runtime_binary);
        command.args(["start", "--attach", &self.container_id]);

        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(ContainerError::UnableToExecuteCommand)?;

        let mut stdout =
            BufReader::new(child.stdout.take().expect("stdout should be piped")).lines();
        let stderr = BufReader::new(child.stderr.take().expect("stderr should be piped")).lines();

        let first_line = tokio::time::timeout(Duration::from_secs(30), async {
            stdout.next_line().await.map_err(|error| {
                ContainerError::UnableReadCommandStdout(format_command(&command), error)
            })
        })
        .await
        .map_err(|_| {
            ContainerError::UnsuccesfulCommandOutput(
                format_command(&command),
                "timeout reached for reading first line".into(),
            )
        })??
        .ok_or_else(|| {
            ContainerError::UnsuccesfulCommandOutput(
                format_command(&command),
                "unexpected EOF".into(),
            )
        })?;

        let internal_proxy_addr: SocketAddr = first_line
            .parse()
            .map_err(ContainerError::UnableParseProxySocketAddr)?;

        Ok((
            internal_proxy_addr,
            LinesStream::new(stdout).merge(LinesStream::new(stderr)),
        ))
    }
}

type SidecarLogs = tokio_stream::adapters::Merge<
    LinesStream<BufReader<ChildStdout>>,
    LinesStream<BufReader<ChildStderr>>,
>;
