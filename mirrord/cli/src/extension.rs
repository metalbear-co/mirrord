use mirrord_analytics::{AnalyticsError, AnalyticsReporter, Reporter};
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_progress::{JsonProgress, Progress, ProgressTracker};

use crate::{
    CliResult, config::ExtensionExecArgs, execution::MirrordExecution, print_config,
    user_data::UserData,
};

/// Actually facilitate execution after all preparations were complete
async fn mirrord_exec<P>(
    #[cfg(target_os = "macos")] executable: Option<&str>,
    mut config: LayerConfig,
    mut progress: P,
    analytics: &mut AnalyticsReporter,
    config_file_path: Option<&str>,
) -> CliResult<()>
where
    P: Progress,
{
    let execution_info = MirrordExecution::start_internal(
        &mut config,
        #[cfg(target_os = "macos")]
        executable,
        #[cfg(target_os = "macos")]
        None,
        &mut progress,
        analytics,
        None,
    )
    .await?;

    // Check if MIRRORD_EXT_PRINT_CONFIG is set to TRUE and print config if so
    if std::env::var("MIRRORD_EXT_PRINT_CONFIG")
        .ok()
        .map(|v| v.to_uppercase() == "TRUE")
        .unwrap_or(false)
    {
        let mut sub_progress_config = progress.subtask("config summary");
        print_config(
            &sub_progress_config,
            None, // No command in extension context
            &config,
            config_file_path,
            execution_info.uses_operator,
        );
        sub_progress_config.success(Some("config summary"));
    }

    let output = serde_json::to_string(&execution_info)?;
    progress.success(Some(&output));
    execution_info.wait().await?;

    Ok(())
}

/// Facilitate the execution of a process using mirrord by an IDE extension
pub(crate) async fn extension_exec(
    args: ExtensionExecArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    let progress = ProgressTracker::try_from_env("mirrord preparing to launch")
        .unwrap_or_else(|| JsonProgress::new("mirrord preparing to launch").into());

    let mut cfg_context = ConfigContext::default()
        .override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file.clone())
        .override_env_opt("MIRRORD_IMPERSONATED_TARGET", args.target);

    let mut config = LayerConfig::resolve(&mut cfg_context)?;
    crate::profile::apply_profile_if_configured(&mut config, &progress).await?;

    let mut analytics = AnalyticsReporter::only_error(
        config.telemetry,
        Default::default(),
        watch,
        user_data.machine_id(),
    );

    analytics
        .get_mut()
        .add("key_length", config.key.analytics_len());

    let result = config.verify(&mut cfg_context);
    for warning in cfg_context.into_warnings() {
        progress.warning(&warning);
    }
    result?;

    #[cfg(target_os = "linux")]
    {
        use std::path::Path;

        use crate::is_static;

        let is_static = args
            .executable
            .as_deref()
            .map(Path::new)
            .is_some_and(is_static::is_binary_static);
        if is_static {
            progress
                .warning("Target binary might not be dynamically linked. mirrord might not work!");
        }
    }

    #[cfg(target_os = "macos")]
    let execution_result = mirrord_exec(
        args.executable.as_deref(),
        config,
        progress,
        &mut analytics,
        args.config_file.as_ref().and_then(|p| p.to_str()),
    )
    .await;
    #[cfg(not(target_os = "macos"))]
    let execution_result = mirrord_exec(
        config,
        progress,
        &mut analytics,
        args.config_file.as_ref().and_then(|p| p.to_str()),
    )
    .await;

    if execution_result.is_err() && !analytics.has_error() {
        analytics.set_error(AnalyticsError::Unknown);
    }

    execution_result
}
