//! Windows-specific logging and tracing initialization.
//!
//! This module handles the setup of tracing/logging for the mirrord Windows layer,
//! supporting multiple output targets including files, console, and stderr.

use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
};

use chrono::Local;
use mirrord_layer_lib::process::windows::execution::MIRRORD_LAYER_LOG_PATH;
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

/// Initialize logger. Set the logs to go according to the layer's config either to a trace file, to
/// mirrord-console or to stderr.
pub fn init_tracing() {
    if let Ok(console_addr) = std::env::var("MIRRORD_CONSOLE_ADDR") {
        mirrord_console::init_logger(&console_addr).expect("logger initialization failed");
        return;
    }

    let pid = std::process::id();
    let mut log_file_path: Option<String> = None;

    let log_file = match std::env::var(MIRRORD_LAYER_LOG_PATH) {
        Ok(log_dir) => match build_log_file_path(&log_dir, pid) {
            Ok(path) => match open_log_file(&path) {
                Ok(file) => {
                    log_file_path = Some(path);
                    Some(file)
                }
                Err(err) => {
                    eprintln!(
                        "Failed to open log file '{}': {}, falling back to stderr logging.",
                        path, err
                    );
                    None
                }
            },
            Err(err) => {
                eprintln!(
                    "Failed to prepare layer log directory '{}': {}. Falling back to stderr logging.",
                    log_dir, err
                );
                None
            }
        },
        Err(_) => None,
    };

    let prefer_debug_default = log_file.is_some();
    init_subscriber(log_file, prefer_debug_default);

    if let Some(log_file) = log_file_path {
        log_process_start(&log_file, pid);
    }
}

/// Initialize tracing subscriber with optional file + stderr layers.
fn init_subscriber(log_file: Option<File>, prefer_debug_default: bool) {
    let env_filter = if prefer_debug_default && std::env::var_os("RUST_LOG").is_none() {
        tracing_subscriber::EnvFilter::new("debug")
    } else {
        tracing_subscriber::EnvFilter::from_default_env()
    };

    if let Some(file) = log_file {
        tracing_subscriber::registry()
            .with(env_filter.clone())
            .with(
                tracing_subscriber::fmt::layer()
                    .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                    .with_thread_ids(true)
                    .with_ansi(false) // File logs should avoid ANSI escape codes
                    .with_writer(file)
                    .with_file(true)
                    .with_line_number(true)
                    .with_target(true)
                    .compact(),
            )
            .with(
                tracing_subscriber::fmt::layer()
                    .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                    .with_thread_ids(true)
                    .with_writer(std::io::stderr)
                    .with_file(true)
                    .with_line_number(true)
                    .pretty(),
            )
            .init();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                    .with_thread_ids(true)
                    .with_writer(std::io::stderr)
                    .with_file(true)
                    .with_line_number(true)
                    .pretty(),
            )
            .init();
    }
}

/// Log process info for file-based logging to help with debugging
fn log_process_start(log_file: &str, pid: u32) {
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

/// Open a log file for writing, truncating existing content.
fn open_log_file(path: &str) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

/// Build the log file path inside the provided directory, ensuring it exists.
fn build_log_file_path(log_dir: &str, pid: u32) -> io::Result<String> {
    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    let dir_path = Path::new(log_dir);

    if dir_path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "log directory path is empty",
        ));
    }

    std::fs::create_dir_all(dir_path)?;

    let file_name = format!("mirrord-layer_{}_pid{}", timestamp, pid);
    let full_path = dir_path.join(file_name);

    Ok(full_path.to_string_lossy().into_owned())
}
