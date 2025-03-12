use mirrord_analytics::{AnalyticsError, AnalyticsReporter, Reporter};
use mirrord_config::LayerConfig;
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
    // extension needs more timeout since it might need to build
    // or run tasks before actually launching.
    #[cfg(target_os = "macos")]
    let mut execution_info =
        MirrordExecution::start(&mut config, executable, &mut progress, analytics).await?;
    #[cfg(not(target_os = "macos"))]
    let execution_info = MirrordExecution::start(&mut config, &mut progress, analytics).await?;

    let output = serde_json::to_string(&execution_info)?;
    progress.success(Some(&output));
    execution_info.wait().await?;

    Ok(())
}

/// Facilitate the execution of a process using mirrord by an IDE extension
pub(crate) async fn extension_exec(args: ExtensionExecArgs, watch: drain::Watch) -> CliResult<()> {
    let progress = ProgressTracker::try_from_env("mirrord preparing to launch")
        .unwrap_or_else(|| JsonProgress::new("mirrord preparing to launch").into());

    // Set environment required for `LayerConfig::from_env_with_warnings`.
    if let Some(config_file) = args.config_file.as_ref() {
        std::env::set_var(LayerConfig::FILE_PATH_ENV, config_file);
    }
    if let Some(target) = args.target.as_ref() {
        std::env::set_var("MIRRORD_IMPERSONATED_TARGET", target.clone());
    }

    let (config, mut context) = LayerConfig::resolve()?;

    let mut analytics = AnalyticsReporter::only_error(config.telemetry, Default::default(), watch);

    config.verify(&mut context)?;
    for warning in context.get_warnings() {
        progress.warning(warning);
    }

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
