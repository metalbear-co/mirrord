use std::{
    ffi::OsStr,
    io::{BufRead, BufReader, Cursor},
    net::SocketAddr,
    path::Path,
};

use exec::execvp;
use mirrord_analytics::{AnalyticsError, AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::LayerConfig;
use mirrord_progress::{Progress, ProgressTracker};
use tokio::process::Command;

use crate::{
    config::{ContainerArgs, ContainerCommand},
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    container::command_builder::RuntimeCommandBuilder,
    error::Result,
    execution::MirrordExecution,
};

mod command_builder;

fn add_config_path<P>(command: &mut RuntimeCommandBuilder, config_path: P)
where
    P: AsRef<OsStr>,
{
    let host_path = config_path
        .as_ref()
        .to_str()
        .expect("should convert")
        .to_owned();

    let config_name = Path::new(&config_path)
        .file_name()
        .and_then(|os_str| os_str.to_str())
        .unwrap_or_default();

    let continer_path = format!("/tmp/{}", config_name);

    command.add_env("MIRRORD_CONFIG_FILE", &continer_path);
    command.add_volume(host_path, continer_path);
}

pub(crate) async fn container_command(args: ContainerArgs, watch: drain::Watch) -> Result<()> {
    let progress = ProgressTracker::from_env("mirrord container");

    let mut config_env_values = args.params.to_env()?;

    for (name, value) in &config_env_values {
        std::env::set_var(name, value);
    }

    let (config, mut context) = LayerConfig::from_env_with_warnings()?;

    let mut analytics = AnalyticsReporter::only_error(config.telemetry, watch);
    (&config).collect_analytics(analytics.get_mut());

    config.verify(&mut context)?;
    for warning in context.get_warnings() {
        progress.warning(warning);
    }

    let mut sub_progress = progress.subtask("preparing to launch process");

    let execution_info =
        MirrordExecution::start_ext(&config, &mut sub_progress, &mut analytics).await?;

    sub_progress.success(None);

    let mut runtime_command = RuntimeCommandBuilder::new(args.runtime);
    let mut sidecar_command = RuntimeCommandBuilder::new(args.runtime);

    runtime_command.add_env("MIRRORD_PROGRESS_MODE", "off");
    sidecar_command.add_env("MIRRORD_PROGRESS_MODE", "off");

    if let Some(config_path) = config_env_values.remove("MIRRORD_CONFIG_FILE") {
        add_config_path(&mut runtime_command, &config_path);
        add_config_path(&mut sidecar_command, &config_path);
    }

    runtime_command.add_envs(config_env_values.iter());
    sidecar_command.add_envs(config_env_values);
    sidecar_command.add_env("MIRRORD_INTPROXY_DETACH_IO", "false");

    sidecar_command.add_envs(
        execution_info
            .environment
            .iter()
            .filter(|(key, _)| key != &"HOSTNAME"),
    );

    let (sidecar_binary, sidecar_args) = sidecar_command
        .with_command(ContainerCommand::Run {
            runtime_args: vec![
                "--rm".to_string(),
                "-d".to_string(),
                args.cli_image.clone(),
                "mirrord".to_string(),
                "intproxy".to_string(),
            ],
        })
        .into_execvp_args();

    let sidecar = Command::new(sidecar_binary)
        .args(&sidecar_args[1..])
        .output()
        .await
        .expect("Sidecar failure");

    let container_id = BufReader::new(Cursor::new(sidecar.stdout))
        .lines()
        .next()
        .transpose()
        .unwrap()
        .unwrap();

    let sidecar_log = Command::new(args.runtime.to_string())
        .args(["logs", &container_id])
        .output()
        .await
        .unwrap();

    let intproxt_socket: SocketAddr = BufReader::new(Cursor::new(sidecar_log.stdout))
        .lines()
        .next()
        .transpose()
        .unwrap()
        .unwrap()
        .parse()
        .unwrap();

    runtime_command.push_arg("--network");
    runtime_command.push_arg(format!("container:{container_id}"));
    runtime_command.push_arg("--volumes-from");
    runtime_command.push_arg(&container_id);

    runtime_command.add_env("LD_PRELOAD", "/opt/mirrord/lib/libmirrord_layer.so");

    runtime_command.add_envs(
        execution_info
            .environment
            .iter()
            .filter(|(key, _)| {
                key != &"HOSTNAME"
                    && key != &"MIRRORD_CONNECT_TCP"
                    && key != &AGENT_CONNECT_INFO_ENV_KEY
            })
            .chain([(
                &"MIRRORD_CONNECT_TCP".to_owned(),
                &intproxt_socket.to_string(),
            )]),
    );

    let (binary, binary_args) = runtime_command
        .with_command(args.command)
        .into_execvp_args();

    let err = execvp(binary, binary_args);
    tracing::error!("Couldn't execute {:?}", err);

    analytics.set_error(AnalyticsError::BinaryExecuteFailed);

    // Kills the intproxy, freeing the agent.
    execution_info.stop().await;

    Ok(())
}
