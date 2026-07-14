use std::{
    fmt,
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    time::SystemTime,
};

use fs4::fs_std::FileExt;

/// Logger used in SIP-patch flow.
///
/// [`tracing`] consumes a lot of stack space and is known to cause problems in the SIP-patch flow
/// (which happens in a dangerous fork/exec).
pub struct SipLogger(Option<File>);

impl SipLogger {
    /// Creates a noop logger that ignores logs.
    pub fn noop() -> Self {
        Self(None)
    }

    /// Creates a logger that will append logs to the file at the given path.
    ///
    /// 1. If the file does not exist, it will be created.
    /// 2. File access will be synchronized with filesystem locks.
    pub fn new_at(path: &Path) -> Self {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .inspect_err(|error| {
                eprintln!("Failed to open SIP log file at {path:?}: {error}");
            })
            .ok();
        Self(file)
    }

    pub fn lock(&mut self) -> SipLoggerGuard<'_> {
        if let Some(file) = &mut self.0 {
            match file.lock_exclusive() {
                Ok(()) => SipLoggerGuard(Some(file)),
                Err(error) => {
                    eprintln!("Failed to acquire exclusive lock on SIP log file: {error}");
                    SipLoggerGuard(None)
                }
            }
        } else {
            SipLoggerGuard(None)
        }
    }
}

pub struct SipLoggerGuard<'a>(Option<&'a mut File>);

impl SipLoggerGuard<'_> {
    pub fn log<M: fmt::Display>(&mut self, message: M) {
        let Some(file) = &mut self.0 else {
            return;
        };

        let pid = std::process::id();
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let result = writeln!(file, "[{timestamp}] [PID={pid}] {message}");
        if let Err(error) = result {
            eprintln!("Failed to write to SIP log file: {error}");
        }
    }

    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }
}

impl Drop for SipLoggerGuard<'_> {
    fn drop(&mut self) {
        if let Some(file) = &mut self.0
            && let Err(error) = file.unlock()
        {
            eprintln!("Failed to release exclusive lock on SIP log file: {error}");
        }
    }
}
