//! Shared logging initialization for layer (unix) and layer-win (windows).

use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
};

use chrono::Local;
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

/// Environment variable for specifying layer log directory path
pub const MIRRORD_LAYER_LOG_PATH: &str = "MIRRORD_LAYER_LOG_PATH";

/// Initialize logger. Set the logs to go according to the layer's config either to a trace
/// file, to mirrord-console or to stderr.
pub fn init_tracing() {
    if let Ok(console_addr) = std::env::var("MIRRORD_CONSOLE_ADDR") {
        mirrord_console::init_logger(&console_addr).expect("logger initialization failed");
        return;
    }

    let log_file = std::env::var(MIRRORD_LAYER_LOG_PATH)
        .ok()
        .and_then(|log_dir| {
            open_log_file_from_env(&log_dir)
                .map_err(|err| {
                    eprintln!(
                        "Failed to open log file from MIRRORD_LAYER_LOG_PATH (error: {})",
                        err
                    );
                    err
                })
                .ok()
        });
    init_subscriber(log_file);
}

/// Initialize tracing subscriber with optional file + stderr layers.
fn init_subscriber(log_file: Option<File>) {
    let registry =
        tracing_subscriber::registry().with(tracing_subscriber::EnvFilter::from_default_env());

    let file_layer = log_file.map(|file| {
        tracing_subscriber::fmt::layer()
            .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
            .with_thread_ids(true)
            .with_ansi(false) // File logs should avoid ANSI escape codes
            .with_writer(file)
            .with_file(true)
            .with_line_number(true)
            .with_target(true)
            .compact()
    });

    // Always add stderr layer
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .with_thread_ids(true)
        .compact()
        .with_writer(std::io::stderr);

    // Note (Daniel): to disable ansi code properly in file, stderr must be last
    // according to this Stackoverflow comment:
    // https://stackoverflow.com/questions/79118770/strange-symbols-ansi-in-a-log-file-when-using-tracing-subscriber#comment139523806_79119452
    registry.with(file_layer).with(stderr_layer).init();
}

fn open_log_file_from_env(log_dir: &str) -> io::Result<File> {
    let path = build_log_file_path(log_dir)?;
    // Open a log file for writing, truncating existing content.
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

/// Build the log file path inside the provided directory, ensuring it exists.
fn build_log_file_path(log_dir: &str) -> io::Result<String> {
    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    let pid = std::process::id();
    let process_name = sanitized_process_name();

    let dir_path = Path::new(log_dir);
    if dir_path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "log directory path is empty",
        ));
    }
    std::fs::create_dir_all(dir_path)?;

    let file_name = format!("mirrord-layer_{}_{}_pid{}", timestamp, process_name, pid);
    let full_path = dir_path.join(file_name);

    eprintln!(
        "mirrord-layer initializing file logger for process '{}' (pid={}), logging to file: {:?}",
        process_name, pid, full_path
    );

    Ok(full_path.to_string_lossy().into_owned())
}

fn sanitized_process_name() -> String {
    let raw_name = std::env::current_exe()
        .ok()
        .and_then(|path| path.file_stem()?.to_str().map(String::from))
        .unwrap_or_else(|| "unknown".to_string());

    let mut sanitized: String = raw_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();

    if sanitized.is_empty() {
        sanitized = "unknown".to_string();
    }

    sanitized
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn init_tracing_creates_log_file() {
        let temp_dir = tempdir().expect("temp dir");

        let prev_log_path = std::env::var(MIRRORD_LAYER_LOG_PATH).ok();
        let prev_console_addr = std::env::var("MIRRORD_CONSOLE_ADDR").ok();
        let prev_log_level = std::env::var("RUST_LOG").ok();

        unsafe {
            std::env::remove_var("MIRRORD_CONSOLE_ADDR");
            std::env::set_var(MIRRORD_LAYER_LOG_PATH, temp_dir.path());
            std::env::set_var("RUST_LOG", "debug");
        }

        init_tracing();
        tracing::info!("logging smoke test");

        std::thread::sleep(Duration::from_millis(20));

        let mut entries = fs::read_dir(temp_dir.path())
            .expect("read temp log dir")
            .filter_map(|entry| entry.ok())
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.path());

        let log_path = entries
            .first()
            .map(|entry| entry.path())
            .expect("log file not created");
        let metadata = fs::metadata(&log_path).expect("log file metadata");
        assert!(
            metadata.len() > 0,
            "expected log file to be non-empty: {}",
            log_path.display()
        );

        unsafe {
            match prev_log_path {
                Some(value) => std::env::set_var(MIRRORD_LAYER_LOG_PATH, value),
                None => std::env::remove_var(MIRRORD_LAYER_LOG_PATH),
            }
            match prev_console_addr {
                Some(value) => std::env::set_var("MIRRORD_CONSOLE_ADDR", value),
                None => std::env::remove_var("MIRRORD_CONSOLE_ADDR"),
            }
            match prev_log_level {
                Some(value) => std::env::set_var("RUST_LOG", value),
                None => std::env::remove_var("RUST_LOG"),
            }
        }
    }
}
