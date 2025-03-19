use std::{fs::OpenOptions, future::Future, ops::Not, path::Path};

use futures::StreamExt;
use mirrord_config::LayerConfig;
use tokio::io::AsyncWriteExt;
use tokio_stream::Stream;
use tracing_subscriber::{prelude::*, EnvFilter};

use crate::{
    config::Commands,
    error::{CliError, ExternalProxyError, InternalProxyError},
};

/// Tries to initialize tracing in the current process.
pub async fn init_tracing_registry(
    command: &Commands,
    watch: drain::Watch,
) -> Result<(), CliError> {
    // Logging to the mirrord-console always takes precedence.
    if let Ok(console_addr) = std::env::var("MIRRORD_CONSOLE_ADDR") {
        mirrord_console::init_async_logger(&console_addr, watch.clone(), 124).await?;

        return Ok(());
    }

    let do_init = match command {
        // These commands are usually called from the plugins.
        //
        // They should log only when explicitly instructed.
        Commands::ListTargets(_) | Commands::ExtensionExec(_) | Commands::ExtensionContainer(_) => {
            // The final error has to be in the JSON format.
            let _ = miette::set_hook(Box::new(|_| Box::new(miette::JSONReportHandler::new())));

            std::env::var("MIRRORD_FORCE_LOG")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(false)
        }

        // Proxies initialize tracing independently, after log file setup.
        Commands::InternalProxy { .. } | Commands::ExternalProxy { .. } => false,

        _ => true,
    };

    if do_init {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_file(true)
                    .with_line_number(true)
                    .pretty(),
            )
            .with(tracing_subscriber::EnvFilter::from_default_env())
            .init();
    }

    Ok(())
}

/// Initializes mirrord intproxy/extproxy tracing registry.
///
/// Fails if the specified log file cannot be opened/created for writing.
///
/// # Log format
///
/// Proxies output logs in JSON, which is not really human-readable.
/// However, it allows us to use some nice tools, like [hl](https://github.com/pamburus/hl).
fn init_proxy_tracing_registry(
    log_destination: &Path,
    log_level: Option<&str>,
    json_log: bool,
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

    let fmt_layer_base = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_file(true)
        .with_line_number(true)
        .with_writer(output_file);
    let fmt_layer = if json_log {
        fmt_layer_base.json().boxed()
    } else {
        fmt_layer_base.pretty().boxed()
    };

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(env_filter)
        .init();

    Ok(())
}

pub fn init_intproxy_tracing_registry(config: &LayerConfig) -> Result<(), InternalProxyError> {
    if crate::util::intproxy_container_mode().not() {
        // When the intproxy does not run in a sidecar container, it logs to a file.
        let log_destination = &config.internal_proxy.log_destination;
        init_proxy_tracing_registry(
            &log_destination,
            config.internal_proxy.log_level.as_deref(),
            config.internal_proxy.json_log,
        )
        .map_err(|fail| {
            InternalProxyError::OpenLogFile(log_destination.to_string_lossy().to_string(), fail)
        })
    } else {
        // When the intproxy runs in a sidecar container, it logs directly to stderr.
        // The logs are then piped to the log file on the host.

        let env_filter = config
            .internal_proxy
            .log_level
            .as_ref()
            .map(|log_level| EnvFilter::builder().parse_lossy(log_level))
            .unwrap_or_else(EnvFilter::from_default_env);

        let fmt_layer_base = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_file(true)
            .with_line_number(true)
            .with_writer(std::io::stderr);
        let fmt_layer = if config.internal_proxy.json_log {
            fmt_layer_base.json().boxed()
        } else {
            fmt_layer_base.pretty().boxed()
        };

        tracing_subscriber::registry()
            .with(fmt_layer)
            .with(env_filter)
            .init();

        Ok(())
    }
}

pub fn init_extproxy_tracing_registry(config: &LayerConfig) -> Result<(), ExternalProxyError> {
    let log_destination = &config.external_proxy.log_destination;
    init_proxy_tracing_registry(
        log_destination,
        config.external_proxy.log_level.as_deref(),
        config.external_proxy.json_log,
    )
    .map_err(|fail| {
        ExternalProxyError::OpenLogFile(log_destination.to_string_lossy().to_string(), fail)
    })
}

pub async fn pipe_intproxy_sidecar_logs<'s, S>(
    config: &LayerConfig,
    stream: S,
) -> Result<impl Future<Output = ()> + 's, InternalProxyError>
where
    S: Stream<Item = std::io::Result<String>> + 's,
{
    let log_destination = &config.internal_proxy.log_destination;
    let mut output_file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_destination)
        .await
        .map_err(|fail| {
            InternalProxyError::OpenLogFile(log_destination.to_string_lossy().to_string(), fail)
        })?;

    Ok(async move {
        let mut stream = std::pin::pin!(stream);

        while let Some(line) = stream.next().await {
            let result: std::io::Result<_> = try {
                output_file.write_all(line?.as_bytes()).await?;
                output_file.write_u8(b'\n').await?;

                output_file.flush().await?;
            };

            if let Err(error) = result {
                tracing::error!(?error, "unable to pipe logs from intproxy");
            }
        }
    })
}
