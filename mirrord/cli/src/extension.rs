use std::collections::HashMap;

use mirrord_analytics::{AnalyticsError, AnalyticsReporter};
use mirrord_config::LayerConfig;
use mirrord_progress::{JsonProgress, Progress, ProgressTracker};

use crate::{config::ExtensionExecArgs, error::CliError, execution::MirrordExecution, Result};

/// Actualy facilitate execution after all preperatations were complete
async fn mirrord_exec<P>(
    #[cfg(target_os = "macos")] executable: Option<&str>,
    env: HashMap<String, String>,
    config: LayerConfig,
    mut progress: P,
    analytics: &mut AnalyticsReporter,
) -> Result<()>
where
    P: Progress + Send + Sync,
{
    // extension needs more timeout since it might need to build
    // or run tasks before actually launching.
    #[cfg(target_os = "macos")]
    let mut execution_info =
        MirrordExecution::start(&config, executable, &progress, analytics).await?;
    #[cfg(not(target_os = "macos"))]
    let mut execution_info = MirrordExecution::start(&config, &progress, analytics).await?;

    // We don't execute so set envs aren't passed, so we need to add config file and target to
    // env.
    execution_info.environment.extend(env);

    let output = serde_json::to_string(&execution_info)?;
    progress.success(Some(&output));
    execution_info.wait().await?;

    Ok(())
}

/// Facilitate the execution of a process using mirrord by an IDE extension
pub(crate) async fn extension_exec(args: ExtensionExecArgs) -> Result<()> {
    let progress = ProgressTracker::try_from_env("mirrord preparing to launch")
        .unwrap_or_else(|| JsonProgress::new("mirrord preparing to launch").into());
    let mut env: HashMap<String, String> = HashMap::new();

    if let Some(config_file) = args.config_file.as_ref() {
        // Set canoncialized path to config file, in case forks/children are in different
        // working directories.
        let full_path = std::fs::canonicalize(config_file)
            .map_err(|e| CliError::ConfigFilePathError(config_file.into(), e))?;
        std::env::set_var("MIRRORD_CONFIG_FILE", full_path.clone());
        env.insert(
            "MIRRORD_CONFIG_FILE".into(),
            full_path.to_string_lossy().into(),
        );
    }
    if let Some(target) = args.target.as_ref() {
        std::env::set_var("MIRRORD_IMPERSONATED_TARGET", target.clone());
        env.insert("MIRRORD_IMPERSONATED_TARGET".into(), target.to_string());
    }
    let (config, mut context) = LayerConfig::from_env_with_warnings()?;

    let mut analytics = AnalyticsReporter::only_error(config.telemetry);

    config.verify(&mut context)?;
    for warning in context.get_warnings() {
        progress.warning(warning);
    }

    #[cfg(target_os = "macos")]
    let execution_result = mirrord_exec(
        args.executable.as_deref(),
        env,
        config,
        progress,
        &mut analytics,
    )
    .await;
    #[cfg(not(target_os = "macos"))]
    let execution_result = mirrord_exec(env, config, progress, &mut analytics).await;

    if execution_result.is_err() && !analytics.has_error() {
        analytics.set_error(AnalyticsError::Unknown);
    }

    execution_result
}
