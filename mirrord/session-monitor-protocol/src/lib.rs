use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    #[serde(default)]
    pub parent_pid: Option<u32>,
    pub process_name: String,
    #[serde(default)]
    pub cmdline: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortSubscription {
    pub port: u16,
    pub mode: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    #[serde(default)]
    pub key: Option<String>,
    pub target: String,
    #[serde(default)]
    pub namespace: Option<String>,
    pub started_at: String,
    pub mirrord_version: String,
    pub is_operator: bool,
    #[serde(default)]
    pub processes: Vec<ProcessInfo>,
    #[serde(default)]
    pub port_subscriptions: Vec<PortSubscription>,
    pub config: serde_json::Value,
}
