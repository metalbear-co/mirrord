use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    time::SystemTime,
};

use mirrord_config::LayerConfig;
use rand::{distributions::Alphanumeric, Rng};
use tracing_subscriber::{prelude::*, EnvFilter};

use crate::{
    config::Commands,
    error::{CliError, ExternalProxyError, InternalProxyError},
};

// only ls and ext commands need the errors in json format
// error logs are disabled for extensions
fn init_ext_error_handler(commands: &Commands) -> bool {
    match commands {
        Commands::ListTargets(_) | Commands::ExtensionExec(_) => {
            let _ = miette::set_hook(Box::new(|_| Box::new(miette::JSONReportHandler::new())));

            true
        }
        _ => false,
    }
}

pub async fn init_tracing_registry(
    command: &Commands,
    watch: drain::Watch,
) -> Result<(), CliError> {
    if let Ok(console_addr) = std::env::var("MIRRORD_CONSOLE_ADDR") {
        mirrord_console::init_async_logger(&console_addr, watch.clone(), 124).await?;

        return Ok(());
    }

    if matches!(
        command,
        Commands::InternalProxy { .. } | Commands::ExternalProxy { .. }
    ) {
        return Ok(());
    }

    // There are situations where even if running "ext" commands that shouldn't log, we want those
    // to log to be able to debug issues.
    let force_log = std::env::var("MIRRORD_FORCE_LOG")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(false);

    if force_log || init_ext_error_handler(command) {
        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .with(tracing_subscriber::EnvFilter::from_default_env())
            .init();
    }

    Ok(())
}

fn default_logfile_path(prefix: &str) -> PathBuf {
    let random_name: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(7)
        .map(char::from)
        .collect();
    let timestamp = SystemTime::UNIX_EPOCH.elapsed().unwrap().as_secs();

    PathBuf::from(format!("/tmp/{prefix}-{timestamp}-{random_name}.log"))
}

fn init_proxy_tracing_registry(
    log_destination: &Path,
    log_level: Option<&str>,
) -> std::io::Result<()> {
    if std::env::var("MIRRORD_CONSOLE_ADDR").is_ok() {
        return Ok(());
    }

    let output_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_destination)?;

    let env_filter = log_level
        .map(|log_level| EnvFilter::builder().parse_lossy(log_level))
        .unwrap_or_else(EnvFilter::from_default_env);

    tracing_subscriber::fmt()
        .with_writer(output_file)
        .with_ansi(false)
        .with_env_filter(env_filter)
        .pretty()
        .init();

    Ok(())
}

pub fn init_intproxy_tracing_registry(config: &LayerConfig) -> Result<(), InternalProxyError> {
    if !config.internal_proxy.container_mode {
        // Setting up default logging for intproxy.
        let log_destination = config
            .internal_proxy
            .log_destination
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| default_logfile_path("mirrord-intproxy"));

        init_proxy_tracing_registry(&log_destination, config.internal_proxy.log_level.as_deref())
            .map_err(|fail| {
                InternalProxyError::OpenLogFile(log_destination.to_string_lossy().to_string(), fail)
            })
    } else {
        let env_filter = config
            .internal_proxy
            .log_level
            .as_ref()
            .map(|log_level| EnvFilter::builder().parse_lossy(log_level))
            .unwrap_or_else(EnvFilter::from_default_env);

        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .with_env_filter(env_filter)
            .pretty()
            .init();

        Ok(())
    }
}

pub fn init_extproxy_tracing_registry(config: &LayerConfig) -> Result<(), ExternalProxyError> {
    // Setting up default logging for extproxy.
    let log_destination = config
        .external_proxy
        .log_destination
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_logfile_path("mirrord-extproxy"));

    init_proxy_tracing_registry(&log_destination, config.external_proxy.log_level.as_deref())
        .map_err(|fail| {
            ExternalProxyError::OpenLogFile(log_destination.to_string_lossy().to_string(), fail)
        })
}
