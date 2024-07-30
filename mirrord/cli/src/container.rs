use std::{
    io::{BufRead, BufReader, Cursor, Write},
    net::SocketAddr,
};

use exec::execvp;
use mirrord_analytics::{AnalyticsError, AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::LayerConfig;
use mirrord_progress::{Progress, ProgressTracker};
use tokio::process::Command;
use tracing::Level;

use crate::{
    config::{ContainerArgs, ContainerCommand},
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    container::command_builder::RuntimeCommandBuilder,
    error::Result,
    execution::{MirrordExecution, LINUX_INJECTION_ENV_VAR},
};

mod command_builder;

static MIRRORD_CONNECT_TCP_ENV_VAR: &str = "MIRRORD_CONNECT_TCP";

#[tracing::instrument(level = Level::TRACE, ret)]
async fn exec_and_get_first_line(command: &mut Command) -> Result<Option<String>> {
    let result = command.output().await.expect("TODO: Replace");

    let reader = BufReader::new(Cursor::new(result.stdout));

    reader
        .lines()
        .next()
        .transpose()
        .map_err(|err| todo!("{err}"))
}

/// Create a "sidecar" container that is running `mirrord intproy` that connects to `mirrord
/// extproxy` running on user machine to be used by execution container (via mounting on same
/// network)
async fn create_sidecar_intproxy(
    config: &LayerConfig,
    base_command: &RuntimeCommandBuilder,
    connection_info: Vec<(&str, &str)>,
) -> Result<(String, SocketAddr)> {
    let mut sidecar_command = base_command.clone();

    sidecar_command.add_env("MIRRORD_INTPROXY_DETACH_IO", "false");
    sidecar_command.add_envs(connection_info);

    let sidecar_container_command = ContainerCommand::run(vec![
        "--rm".to_string(),
        "-d".to_string(),
        config.container.cli_image.clone(),
        "mirrord".to_string(),
        "intproxy".to_string(),
    ]);

    let (runtime_binary, sidecar_args) = sidecar_command
        .with_command(sidecar_container_command)
        .into_execvp_args();

    // We skip first index of sidecar_args because `RuntimeCommandBuilder::into_execvp_args` adds
    // the binary as first arg for `execvp`
    let sidecar_container_id =
        exec_and_get_first_line(Command::new(&runtime_binary).args(&sidecar_args[1..]))
            .await?
            .unwrap();

    let intproxt_socket: SocketAddr =
        exec_and_get_first_line(Command::new(runtime_binary).args(["logs", &sidecar_container_id]))
            .await?
            .unwrap()
            .parse()
            .unwrap();

    Ok((sidecar_container_id, intproxt_socket))
}

/// Main entry point for the `mirrord container` command.
/// This spawns: "agent" - "external proxy" - "intproxy sidecar" - "execution container"
pub(crate) async fn container_command(args: ContainerArgs, watch: drain::Watch) -> Result<()> {
    let progress = ProgressTracker::from_env("mirrord container");

    for (name, value) in args.params.as_env_vars()? {
        std::env::set_var(name, value);
    }

    let (config, mut context) = LayerConfig::from_env_with_warnings()?;

    let mut analytics = AnalyticsReporter::only_error(config.telemetry, watch);
    (&config).collect_analytics(analytics.get_mut());

    config.verify(&mut context)?;
    for warning in context.get_warnings() {
        progress.warning(warning);
    }

    let mut temp_config_file = tempfile::NamedTempFile::new().unwrap();
    temp_config_file
        .write_all(&serde_json::to_vec_pretty(&config).unwrap())
        .unwrap();

    let mut sub_progress = progress.subtask("preparing to launch process");

    let execution_info =
        MirrordExecution::start_external(&config, &mut sub_progress, &mut analytics).await?;

    let mut connection_info = Vec::new();
    let mut execution_info_env_without_connection_info = Vec::new();

    for (key, value) in &execution_info.environment {
        if key == MIRRORD_CONNECT_TCP_ENV_VAR || key == AGENT_CONNECT_INFO_ENV_KEY {
            connection_info.push((key.as_str(), value.as_str()));
        } else {
            execution_info_env_without_connection_info.push((key.as_str(), value.as_str()))
        }
    }

    sub_progress.success(None);

    let mut runtime_command = RuntimeCommandBuilder::new(args.runtime);
    runtime_command.add_env("MIRRORD_PROGRESS_MODE", "off");
    runtime_command.add_env("MIRRORD_CONFIG_FILE", "/tmp/mirrord-config.json");
    runtime_command.add_volume(temp_config_file.path(), "/tmp/mirrord-config.json");

    runtime_command.add_envs(execution_info_env_without_connection_info);

    let (sidecar_container_id, sidecar_intproxt_socket) =
        create_sidecar_intproxy(&config, &runtime_command, connection_info).await?;

    runtime_command.add_network(format!("container:{sidecar_container_id}"));
    runtime_command.add_volumes_from(sidecar_container_id);

    runtime_command.add_env(LINUX_INJECTION_ENV_VAR, config.container.cli_image_lib_path);
    runtime_command.add_env(
        MIRRORD_CONNECT_TCP_ENV_VAR,
        sidecar_intproxt_socket.to_string(),
    );

    let (binary, binary_args) = runtime_command
        .with_command(args.command)
        .into_execvp_args();

    let err = execvp(binary, binary_args);
    tracing::error!("Couldn't execute {:?}", err);

    analytics.set_error(AnalyticsError::BinaryExecuteFailed);

    // Kills the intproxy, freeing the agent.
    execution_info.stop().await;
    temp_config_file.close().unwrap();

    Ok(())
}
