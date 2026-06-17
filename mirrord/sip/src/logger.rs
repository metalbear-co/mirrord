use std::{
    fmt,
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    time::SystemTime,
};

use fs4::fs_std::FileExt;

pub struct SipLogger(Option<File>);

impl SipLogger {
    pub fn noop() -> Self {
        Self(None)
    }

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
        let pid = std::process::id();
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        let Some(file) = &mut self.0 else {
            eprintln!("[{pid}] [{timestamp}] {message}");
            return;
        };

        let result = writeln!(file, "[{pid}] [{timestamp}] {message}");
        if let Err(error) = result {
            eprintln!("Failed to write to SIP log file: {error}");
        }
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
