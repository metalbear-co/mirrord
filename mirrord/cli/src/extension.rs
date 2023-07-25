use std::collections::HashMap;

use mirrord_config::LayerConfig;
use mirrord_progress::{Progress, TaskProgress};

use crate::{config::ExtensionExecArgs, error::CliError, execution::MirrordExecution, Result};

/// Facilitate the execution of a process using mirrord by an IDE extension
#[tokio::main]
pub(crate) async fn extension_exec(args: ExtensionExecArgs, progress: &TaskProgress) -> Result<()> {
    let progress = progress.subtask("mirrord preparing to launch");
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

    // extension needs more timeout since it might need to build
    // or run tasks before actually launching.
    #[cfg(target_os = "macos")]
    let mut execution_info =
        MirrordExecution::start(&config, args.executable.as_deref(), &progress, Some(60)).await?;
    #[cfg(not(target_os = "macos"))]
    let mut execution_info = MirrordExecution::start(&config, &progress, Some(60)).await?;

    // We don't execute so set envs aren't passed, so we need to add config file and target to env.
    execution_info.environment.extend(env);

    let output = serde_json::to_string(&execution_info)?;
    progress.done_with(&output);
    execution_info.wait().await?;
    Ok(())
}
