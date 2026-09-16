//! Shared logging initialization for layer (unix) and layer-win (windows).
//!
//! # Warning: this code runs on threads that Rust does not own
//!
//! The layer is a library inside somebody else's process. Its hooks, and on Windows its
//! `DllMain`, run on every thread of that process, at every stage of a thread's life. A thread
//! that Rust is still starting has not claimed its thread handle slot yet. Rust claims that slot
//! with `set_current` as the first act of a spawned thread, and aborts the whole process when it
//! finds the slot already taken: "current thread handle already set during thread spawn". The
//! layer spawns Rust threads of its own (the worker in `layer-win/src/lib.rs` and the
//! `task_pool` workers), so anything that claims the slot early kills the process.
//!
//! Do not use any of these in this file, or in any hook, or in `DllMain`:
//!
//! - `std::thread::current` and `std::thread::park`.
//! - `tracing_subscriber`'s `with_thread_ids` and `with_thread_names`, which call it. Use
//!   [`OsThreadId`] instead.
//! - `std::io::stderr` and `std::io::stdout`, and the `println!` and `eprintln!` macros, which take
//!   a reentrant lock that calls it. Use [`StderrHandle`] instead.
//!
//! Writing to a [`File`] is safe, and so is allocating and formatting.

use std::{
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use chrono::Local;
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};
#[cfg(windows)]
use {
    tracing::{Event, Subscriber},
    tracing_subscriber::{
        fmt::{
            FmtContext, FormatEvent, FormatFields, MakeWriter,
            format::{Format, Writer},
        },
        registry::LookupSpan,
    },
    winapi::um::{
        fileapi::WriteFile, processenv::GetStdHandle, processthreadsapi::GetCurrentThreadId,
        winbase::STD_ERROR_HANDLE,
    },
};

/// Environment variable for specifying layer log directory path
pub const MIRRORD_LAYER_LOG_PATH: &str = "MIRRORD_LAYER_LOG_PATH";

/// The layer log file chosen at init, when file logging is active.
static LOG_FILE_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Returns the layer's log file path, when file logging is active.
///
/// The crash diagnostics register this with the monitor, so a crash bundles the exact file instead
/// of globbing by pid (which could match a prior session's log).
///
/// # Returns
///
/// The log file path, or `None` when logs only go to stderr.
pub fn current_log_file() -> Option<PathBuf> {
    LOG_FILE_PATH.get().cloned()
}

/// Windows-only support for the layer's own logging.
///
/// The layer is a library inside somebody else's process, and its hooks run on that process's
/// threads at every point of their life. That rules out parts of the ordinary logging path, so
/// this module supplies replacements: a thread id that needs no Rust state, and a writer that
/// takes no lock.
#[cfg(windows)]
mod windows_support {
    use super::*;

    /// Stamps the operating system thread id in front of each event, then delegates.
    ///
    /// Replaces `with_thread_ids`, which asks Rust for the current thread handle. See
    /// [`StderrHandle`] for why the layer must never ask. The operating system id needs no Rust
    /// state, and it is the id a debugger or ETW shows for the same thread.
    pub(super) struct OsThreadId<F>(F);

    impl<S, N, F> FormatEvent<S, N> for OsThreadId<F>
    where
        F: FormatEvent<S, N>,
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
        N: for<'writer> FormatFields<'writer> + 'static,
    {
        fn format_event(
            &self,
            context: &FmtContext<'_, S, N>,
            mut writer: Writer<'_>,
            event: &Event<'_>,
        ) -> std::fmt::Result {
            write!(writer, "tid({:>5}) ", unsafe { GetCurrentThreadId() })?;
            self.0.format_event(context, writer.by_ref(), event)
        }
    }

    /// Writes to the standard error handle without going through [`std::io::stderr`].
    ///
    /// The layer's hooks and its `DllMain` run on threads the process is still starting, before the
    /// Rust runtime has claimed that thread's handle slot. `std::io::stderr` takes a reentrant lock
    /// that claims the slot first, and the runtime aborts the whole process the next time it starts
    /// a thread of its own: "current thread handle already set during thread spawn". Writing
    /// straight to the handle keeps the console output and touches no Rust thread state.
    #[derive(Clone, Copy)]
    pub(super) struct StderrHandle;

    impl io::Write for StderrHandle {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let mut written = 0u32;
            let wrote = unsafe {
                WriteFile(
                    GetStdHandle(STD_ERROR_HANDLE),
                    buffer.as_ptr().cast(),
                    buffer.len() as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };

            match wrote {
                0 => Err(io::Error::last_os_error()),
                _ => Ok(written as usize),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for StderrHandle {
        type Writer = StderrHandle;

        fn make_writer(&'writer self) -> Self::Writer {
            *self
        }
    }

    /// Event format for both sinks, with the thread id taken from the operating system.
    ///
    /// No ANSI codes in either sink: the file must not carry escape sequences, and the stderr sink
    /// writes through a raw handle that no terminal has prepared.
    pub(super) fn event_format() -> OsThreadId<Format<tracing_subscriber::fmt::format::Compact>> {
        OsThreadId(
            tracing_subscriber::fmt::format()
                .compact()
                .with_ansi(false)
                .with_file(true)
                .with_line_number(true)
                .with_target(true),
        )
    }
}

#[cfg(windows)]
use windows_support::{StderrHandle, event_format};

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
    let registry = tracing_subscriber::registry().with(
        tracing_subscriber::EnvFilter::builder()
            .with_env_var("MIRRORD_LOG")
            .from_env_lossy(),
    );

    #[cfg(windows)]
    let file_layer = log_file.map(|file| {
        tracing_subscriber::fmt::layer()
            .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
            .with_ansi(false) // File logs should avoid ANSI escape codes
            .with_writer(file)
            .event_format(event_format())
    });

    #[cfg(not(windows))]
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
    #[cfg(windows)]
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .with_writer(StderrHandle)
        .event_format(event_format());

    #[cfg(not(windows))]
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

    // Open for writing, truncating existing content.
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)?;

    // Remember the path so the crash diagnostics can register and bundle this exact file.
    let _ = LOG_FILE_PATH.set(PathBuf::from(path));
    Ok(file)
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
        .unwrap_or_else(|| "unknown".to_owned());

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
        sanitized = "unknown".to_owned();
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
        let prev_log_level = std::env::var("MIRRORD_LOG").ok();
        let prev_rust_log = std::env::var("RUST_LOG").ok();

        unsafe {
            std::env::remove_var("MIRRORD_CONSOLE_ADDR");
            std::env::set_var(MIRRORD_LAYER_LOG_PATH, temp_dir.path());
            std::env::set_var("MIRRORD_LOG", "debug");
            std::env::set_var("RUST_LOG", "off");
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
                Some(value) => std::env::set_var("MIRRORD_LOG", value),
                None => std::env::remove_var("MIRRORD_LOG"),
            }
            match prev_rust_log {
                Some(value) => std::env::set_var("RUST_LOG", value),
                None => std::env::remove_var("RUST_LOG"),
            }
        }
    }
}
