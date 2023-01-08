use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub name: String,
    args: Vec<String>,
    env: Vec<String>,
    cwd: String,
    pub id: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Hello {
    pub process_info: ProcessInfo,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Into<log::Level> for Level {
    fn into(self) -> log::Level {
        match self {
            Level::Debug => log::Level::Debug,
            Level::Warn => log::Level::Warn,
            Level::Info => log::Level::Info,
            Level::Error => log::Level::Error,
            Level::Trace => log::Level::Trace,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub level: Level,
    pub target: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Record {
    pub metadata: Metadata,
    pub message: String,
    pub module_path: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
}
