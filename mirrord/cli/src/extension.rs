use mirrord_analytics::{AnalyticsError, AnalyticsReporter, Reporter};
use mirrord_config::{config::ConfigContext, LayerConfig};
use mirrord_progress::{JsonProgress, Progress, ProgressTracker};

use crate::{config::ExtensionExecArgs, execution::MirrordExecution, CliResult};

/// Actually facilitate execution after all preparations were complete
async fn mirrord_exec<P>(
    #[cfg(target_os = "macos")] executable: Option<&str>,
    mut config: LayerConfig,
    mut progress: P,
    analytics: &mut AnalyticsReporter,
) -> CliResult<()>
where
    P: Progress + Send + Sync,
{
    let execution_info = MirrordExecution::start_internal(
        &mut config,
        #[cfg(target_os = "macos")]
        executable,
        &mut progress,
        analytics,
    )
    .await?;

    let output = serde_json::to_string(&execution_info)?;
    progress.success(Some(&output));
    execution_info.wait().await?;

    Ok(())
}

/// Facilitate the execution of a process using mirrord by an IDE extension
pub(crate) async fn extension_exec(args: ExtensionExecArgs, watch: drain::Watch) -> CliResult<()> {
    let progress = ProgressTracker::try_from_env("mirrord preparing to launch")
        .unwrap_or_else(|| JsonProgress::new("mirrord preparing to launch").into());

    let mut cfg_context = ConfigContext::default()
        .override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file)
        .override_env_opt("MIRRORD_IMPERSONATED_TARGET", args.target);

    let mut config = LayerConfig::resolve(&mut cfg_context)?;
    crate::profile::apply_profile_if_configured(&mut config, &progress).await?;

    let mut analytics = AnalyticsReporter::only_error(config.telemetry, Default::default(), watch);

    let result = config.verify(&mut cfg_context);
    for warning in cfg_context.into_warnings() {
        progress.warning(&warning);
    }
    result?;

    #[cfg(target_os = "macos")]
    let execution_result =
        mirrord_exec(args.executable.as_deref(), config, progress, &mut analytics).await;
    #[cfg(not(target_os = "macos"))]
    let execution_result = mirrord_exec(config, progress, &mut analytics).await;

    if execution_result.is_err() && !analytics.has_error() {
        analytics.set_error(AnalyticsError::Unknown);
    }

    execution_result
}
