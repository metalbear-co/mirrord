use std::path::Path;

use exec::execvp;
use mirrord_analytics::{AnalyticsError, AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::LayerConfig;
use mirrord_progress::{Progress, ProgressTracker};

use crate::{
    config::ContainerArgs,
    error::Result,
    execution::{MirrordExecution, INJECTION_ENV_VAR, LINUX_INJECTION_ENV_VAR},
};

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
    #[cfg(not(target_os = "macos"))]
    let mut execution_info = MirrordExecution::start(&config, &mut sub_progress, analytics).await?;

    tracing::info!(?execution_info, "starting");

    if let Some(connect_tcp) = execution_info.environment.get_mut("MIRRORD_CONNECT_TCP") {
        *connect_tcp = str::replace(connect_tcp, "127.0.0.1", "10.0.0.3");
    }

    #[cfg(target_os = "macos")]
    let library_path = {
        execution_info.environment.remove(INJECTION_ENV_VAR);

        let library_path = "/Users/dmitry/ws/metalbear/mirrord/target/aarch64-unknown-linux-gnu/debug/libmirrord_layer.so".to_owned();

        execution_info.environment.insert(
            LINUX_INJECTION_ENV_VAR.to_string(),
            "/tmp/libmirrord_layer.so".to_string(),
        );

        library_path
    };

    #[cfg(not(target_os = "macos"))]
    let library_path = {
        let path = execution_info
            .environment
            .get(INJECTION_ENV_VAR)
            .cloned()
            .unwrap_or_default();

        (path.clone(), path)
    };

    let mirrord_config_path = config_env
        .iter_mut()
        .find(|(key, _)| key == "MIRRORD_CONFIG_FILE")
        .map(|(_, config_path)| {
            let host_path = config_path.to_str().expect("should convert").to_owned();

            let config_name = Path::new(config_path)
                .file_name()
                .and_then(|os_str| os_str.to_str())
                .unwrap_or_default();

            let continer_path = format!("/tmp/{}", config_name);

            *config_path = (&continer_path).into();

            (host_path, continer_path)
        });

    let mut extra_args = Vec::new();

    for (key, value) in execution_info.environment.iter() {
        if key == "HOSTNAME" {
            continue;
        }

        extra_args.push("-e".to_owned());
        extra_args.push(format!("{key}={value}"));
    }

    for (key, value) in config_env {
        let (Some(key), Some(value)) = (key.to_str(), value.to_str()) else {
            continue;
        };

        extra_args.push("-e".to_owned());
        extra_args.push(format!("{key}={value}"));
    }

    extra_args.push("-v".to_owned());
    extra_args.push(format!("{library_path}:/tmp/libmirrord_layer.so"));

    if let Some((host_path, continer_path)) = mirrord_config_path {
        extra_args.push("-v".to_owned());
        extra_args.push(format!("{host_path}:{continer_path}"));
    }

    tracing::warn!(?extra_args, "adding args");

    let mut runtime_args_iter = args.runtime_args.into_iter().peekable();

    let first_arg: Option<&str> = runtime_args_iter.peek().map(|x| x.as_str());

    assert_eq!(first_arg, Some("run"));

    let runtime_args: Vec<_> = [
        args.runtime.to_string(),
        runtime_args_iter.next().expect("Should Exist"),
    ]
    .into_iter()
    .chain(extra_args)
    .chain(runtime_args_iter)
    .collect();

    let err = execvp(args.runtime.to_string(), runtime_args);
    tracing::error!("Couldn't execute {:?}", err);

    analytics.set_error(AnalyticsError::BinaryExecuteFailed);

    // Kills the intproxy, freeing the agent.
    execution_info.stop().await;

    Ok(())
}
