use std::{env, process};

use bincode::{Decode, Encode};
use log::Level;

#[derive(Debug, Encode, Decode)]
pub struct ProcessInfo {
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub cwd: Option<String>,
    pub id: u64,
}

#[derive(Debug, Encode, Decode)]
pub struct Hello {
    pub process_info: ProcessInfo,
}

impl Hello {
    /// Creates a new [`Hello`] message from the environment of the current process.
    pub fn from_env() -> Self {
        Self {
            process_info: ProcessInfo {
                args: env::args().collect(),
                env: env::vars().map(|(k, v)| format!("{k}={v}")).collect(),
                cwd: env::current_dir()
                    .map(|p| p.to_str().map(String::from))
                    .unwrap_or(None),
                id: process::id().into(),
            },
        }
    }
}

/// Variants of this enum match those of [`Level`].
/// Created because [`Level`] does not implement [`Encode`]/[`Decode`].
#[derive(Debug, Encode, Decode)]
pub enum EncodableLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<Level> for EncodableLevel {
    fn from(value: Level) -> Self {
        match value {
            Level::Debug => Self::Debug,
            Level::Error => Self::Error,
            Level::Info => Self::Info,
            Level::Trace => Self::Trace,
            Level::Warn => Self::Warn,
        }
    }
}

impl From<EncodableLevel> for Level {
    fn from(value: EncodableLevel) -> Self {
        match value {
            EncodableLevel::Debug => Self::Debug,
            EncodableLevel::Error => Self::Error,
            EncodableLevel::Info => Self::Info,
            EncodableLevel::Trace => Self::Trace,
            EncodableLevel::Warn => Self::Warn,
        }
    }
}

#[derive(Debug, Encode, Decode)]
pub struct Metadata {
    pub level: EncodableLevel,
    pub target: String,
}

#[derive(Debug, Encode, Decode)]
pub struct Record {
    pub metadata: Metadata,
    pub message: String,
    pub module_path: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
}

impl<'a> From<&'a log::Record<'_>> for Record {
    fn from(record: &'a log::Record) -> Self {
        Self {
            metadata: Metadata {
                level: record.level().into(),
                target: record.target().to_string(),
            },
            message: record.args().to_string(),
            module_path: record.module_path().map(|s| s.to_string()),
            file: record.file().map(|s| s.to_string()),
            line: record.line(),
        }
    }
}
