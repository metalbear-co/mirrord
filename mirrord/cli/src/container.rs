use std::path::Path;

use exec::execvp;
use mirrord_analytics::{AnalyticsError, AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::LayerConfig;
use mirrord_progress::{Progress, ProgressTracker};

use crate::{
    config::ContainerArgs,
    container::command_builder::RuntimeCommandBuilder,
    error::Result,
    execution::{MirrordExecution, INJECTION_ENV_VAR},
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

    #[cfg(target_os = "macos")]
    let mut execution_info =
        MirrordExecution::start(&config, None, &mut sub_progress, &mut analytics).await?;
    #[cfg(target_os = "linux")]
    let mut execution_info =
        MirrordExecution::start(&config, &mut sub_progress, &mut analytics).await?;

    tracing::info!(?execution_info, "starting");

    let mut command = RuntimeCommandBuilder::new(args.runtime);

    execution_info.environment.remove("HOSTNAME");

    if let Some(connect_tcp) = execution_info.environment.remove("MIRRORD_CONNECT_TCP") {
        command.add_env(
            "MIRRORD_CONNECT_TCP",
            str::replace(&connect_tcp, "127.0.0.1", "10.0.0.4"),
        );
    }

    #[cfg(target_os = "macos")]
    {
        execution_info.environment.remove(INJECTION_ENV_VAR);

        command.add_env(
            crate::execution::LINUX_INJECTION_ENV_VAR,
            "/tmp/libmirrord_layer.so",
        );
        command.add_volume("/Users/dmitry/ws/metalbear/mirrord/target/aarch64-unknown-linux-gnu/debug/libmirrord_layer.so", "/tmp/libmirrord_layer.so");
    }

    #[cfg(target_os = "linux")]
    {
        if let Some((_, library_path)) = execution_info.environment.remove(INJECTION_ENV_VAR) {
            command.add_env(INJECTION_ENV_VAR, "/tmp/libmirrord_layer.so");
            command.add_volume(library_path, "/tmp/libmirrord_layer.so");
        }
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

        command.add_env("MIRRORD_CONFIG_FILE", &continer_path);
        command.add_volume(host_path, continer_path);
    }

    command.add_envs(config_env);
    command.add_envs(&execution_info.environment);

    let (binary, binary_args) = command.with_command(args.command).into_execvp_args();
    let err = execvp(binary, binary_args);
    tracing::error!("Couldn't execute {:?}", err);

    analytics.set_error(AnalyticsError::BinaryExecuteFailed);

    // Kills the intproxy, freeing the agent.
    execution_info.stop().await;

    Ok(())
}
