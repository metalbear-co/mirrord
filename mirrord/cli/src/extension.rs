use std::collections::HashMap;

use mirrord_analytics::{send_analytics, AnalyticsReporter};
use mirrord_config::LayerConfig;
use mirrord_progress::{JsonProgress, Progress, ProgressTracker};

use crate::{config::ExtensionExecArgs, error::CliError, execution::MirrordExecution, Result};

/// Facilitate the execution of a process using mirrord by an IDE extension
pub(crate) async fn extension_exec(args: ExtensionExecArgs) -> Result<()> {
    let mut progress = ProgressTracker::try_from_env("mirrord preparing to launch")
        .unwrap_or_else(|| JsonProgress::new("mirrord preparing to launch").into());
    let mut env: HashMap<String, String> = HashMap::new();

    if let Some(config_file) = args.config_file {
        // Set canoncialized path to config file, in case forks/children are in different
        // working directories.
        let full_path = std::fs::canonicalize(&config_file)
            .map_err(|e| CliError::ConfigFilePathError(config_file.into(), e))?;
        std::env::set_var("MIRRORD_CONFIG_FILE", full_path.clone());
        env.insert(
            "MIRRORD_CONFIG_FILE".into(),
            full_path.to_string_lossy().into(),
        );
    }
    if let Some(target) = args.target {
        std::env::set_var("MIRRORD_IMPERSONATED_TARGET", target.clone());
        env.insert("MIRRORD_IMPERSONATED_TARGET".into(), target);
    }
    let config = LayerConfig::from_env()?;

    let mut analytics = AnalyticsReporter::new(config.telemetry);

    let execution_result = async {
        // extension needs more timeout since it might need to build
        // or run tasks before actually launching.
        #[cfg(target_os = "macos")]
        let mut execution_info = MirrordExecution::start(
            &config,
            args.executable.as_deref(),
            &progress,
            &mut analytics,
        )
        .await?;
        #[cfg(not(target_os = "macos"))]
        let mut execution_info =
            MirrordExecution::start(&config, &progress, &mut analytics).await?;

        // We don't execute so set envs aren't passed, so we need to add config file and target to
        // env.
        execution_info.environment.extend(env);

        let output = serde_json::to_string(&execution_info)?;
        progress.success(Some(&output));
        execution_info.wait().await?;

        Ok(())
    }
    .await;

    if analytics.has_error() {
        send_analytics(analytics).await;
    }

    execution_result
}
