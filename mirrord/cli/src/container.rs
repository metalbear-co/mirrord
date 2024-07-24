use std::{fs::Permissions, os::unix::fs::PermissionsExt, path::Path};

use exec::execvp;
use mirrord_analytics::{AnalyticsError, AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::LayerConfig;
use mirrord_progress::{Progress, ProgressTracker};
use tokio::fs::set_permissions;

use crate::{
    config::ContainerArgs, container::command_builder::RuntimeCommandBuilder, error::Result,
    execution::MirrordExecution,
};

mod command_builder;

pub(crate) async fn container_command(args: ContainerArgs, watch: drain::Watch) -> Result<()> {
    let progress = ProgressTracker::from_env("mirrord container");

    let mut config_env = args.params.to_env()?;

    for (name, value) in &config_env {
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

    tracing::info!(?execution_info, "starting");

    let mut runtime_command = RuntimeCommandBuilder::new(args.runtime);
    runtime_command.add_env("MIRRORD_PROGRESS_MODE", "off");

    #[cfg(target_os = "macos")]
    {
        runtime_command.add_volume(
            "/Users/dmitry/ws/metalbear/mirrord/target/aarch64-unknown-linux-gnu/debug/mirrord",
            "/usr/bin/mirrord",
        );
    }

    if let Some(mirrord_config_path) = config_env.remove("MIRRORD_CONFIG_FILE") {
        let host_path = mirrord_config_path
            .to_str()
            .expect("should convert")
            .to_owned();

        let config_name = Path::new(&mirrord_config_path)
            .file_name()
            .and_then(|os_str| os_str.to_str())
            .unwrap_or_default();

        let continer_path = format!("/tmp/{}", config_name);

        runtime_command.add_env("MIRRORD_CONFIG_FILE", &continer_path);
        runtime_command.add_volume(host_path, continer_path);
    }

    runtime_command.add_envs(config_env);
    runtime_command.add_envs(
        execution_info
            .environment
            .iter()
            .filter(|(key, _)| key != &"HOSTNAME"),
    );

    let mut runtime_command = runtime_command.with_command(args.command);

    let temp_entrypoint = {
        let entrypoint = runtime_command
            .entrypoint()
            .map(|entrypoint| format!("{entrypoint} -- "))
            .unwrap_or_default();

        let temp_entrypoint = tempfile::NamedTempFile::new().unwrap();

        let _ = tokio::fs::write(
            &temp_entrypoint,
            format!("#!/usr/bin/env sh\n\nmirrord exec {entrypoint}$@\n"),
        )
        .await;

        let _ = set_permissions(&temp_entrypoint, Permissions::from_mode(0o511)).await;

        runtime_command.add_volume(&temp_entrypoint, "/tmp/mirrord-entrypoint.sh");
        runtime_command.add_entrypoint("/tmp/mirrord-entrypoint.sh");

        temp_entrypoint
    };

    let (binary, binary_args) = runtime_command.into_execvp_args();

    let err = execvp(binary, binary_args);
    tracing::error!("Couldn't execute {:?}", err);

    analytics.set_error(AnalyticsError::BinaryExecuteFailed);

    // Kills the intproxy, freeing the agent.
    execution_info.stop().await;
    let _ = temp_entrypoint.close();

    Ok(())
}
