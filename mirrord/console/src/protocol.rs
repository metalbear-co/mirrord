use log::Level;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub cwd: Option<String>,
    pub id: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Hello {
    pub process_info: ProcessInfo,
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
