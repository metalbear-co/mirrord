#![cfg(unix)]

use std::path::{Path, PathBuf};

pub use mirrord_session_monitor_protocol::{ProcessInfo, SessionInfo};
use thiserror::Error;

pub struct SessionConnection {
    pub socket_path: PathBuf,
    pub info: SessionInfo,
    pub client: reqwest::Client,
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("unexpected status {0} from session API")]
    BadStatus(reqwest::StatusCode),
}

pub fn sessions_dir() -> Option<PathBuf> {
    home::home_dir().map(|home_dir| home_dir.join(".mirrord").join("sessions"))
}

pub fn session_socket_entries(sessions_dir: &Path) -> Vec<(String, PathBuf)> {
    let entries = match std::fs::read_dir(sessions_dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|extension| extension.to_str()) == Some("sock"))
                .then_some(path)
        })
        .filter_map(|path| {
            let session_id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned)?;

            Some((session_id, path))
        })
        .collect()
}

pub fn unix_client(socket_path: &Path) -> Result<reqwest::Client, SessionError> {
    Ok(reqwest::Client::builder()
        .unix_socket(socket_path)
        .build()?)
}

pub async fn fetch_session_info(client: &reqwest::Client) -> Result<SessionInfo, SessionError> {
    let response = client.get("http://localhost/info").send().await?;
    if !response.status().is_success() {
        return Err(SessionError::BadStatus(response.status()));
    }

    Ok(response.json().await?)
}

pub async fn connect_to_session(socket_path: &Path) -> Result<SessionConnection, SessionError> {
    let client = unix_client(socket_path)?;
    let info = fetch_session_info(&client).await?;

    Ok(SessionConnection {
        socket_path: socket_path.to_path_buf(),
        info,
        client,
    })
}

pub async fn kill_session(client: &reqwest::Client) -> Result<(), SessionError> {
    let response = client.post("http://localhost/kill").send().await?;
    if !response.status().is_success() {
        return Err(SessionError::BadStatus(response.status()));
    }

    Ok(())
}
