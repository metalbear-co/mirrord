use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct ProcessInfo {
    name: String,
    args: Vec<String>,
    env: Vec<String>,
    cwd: String,
    id: u64
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Hello {
    process_info: ProcessInfo,
}