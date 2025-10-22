//! Windows-specific logging and tracing initialization.
//!
//! This module handles the setup of tracing/logging for the mirrord Windows layer,
//! supporting multiple output targets including files, console, and stderr.

use std::fs::OpenOptions;

use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

/// Environment variable for specifying layer log file path
const MIRRORD_LAYER_LOG_FILE: &str = "MIRRORD_LAYER_LOG_FILE";

/// Initialize logger. Set the logs to go according to the layer's config either to a trace file, to
/// mirrord-console or to stderr.
pub fn init_tracing() {
    if let Ok(console_addr) = std::env::var("MIRRORD_CONSOLE_ADDR") {
        mirrord_console::init_logger(&console_addr).expect("logger initialization failed");
    } else if let Ok(log_file) = std::env::var(MIRRORD_LAYER_LOG_FILE) {
        // File-based logging for child processes where stderr isn't visible
        init_file_tracing(&log_file);
    } else {
        init_stderr_tracing();
    }
}

/// Initialize file-based tracing with process information logging
fn init_file_tracing(log_file: &str) {
    match OpenOptions::new().create(true).append(true).open(log_file) {
        Ok(file) => {
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                        .with_thread_ids(true)
                        .with_writer(file)
                        .with_file(true)
                        .with_line_number(true)
                        .with_target(true)
                        .compact(),
                )
                .with(tracing_subscriber::EnvFilter::from_default_env())
                .init();

            // Log the process info to help with debugging
            let pid = std::process::id();
            let process_name = std::env::current_exe()
                .ok()
                .and_then(|path| path.file_stem()?.to_str().map(String::from))
                .unwrap_or_else(|| "unknown".to_string());

            tracing::info!(
                "mirrord-layer-win initialized for process '{}' (pid={}), logging to file: {}",
                process_name,
                pid,
                log_file
            );
        }
        Err(e) => {
            eprintln!(
                "Failed to open log file '{}': {}, falling back to stderr",
                log_file, e
            );
            init_stderr_tracing();
        }
    }
}

/// Initialize stderr-based tracing with pretty formatting
fn init_stderr_tracing() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                .with_thread_ids(true)
                .with_writer(std::io::stderr)
                .with_file(true)
                .with_line_number(true)
                .pretty(),
        )
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();
}
