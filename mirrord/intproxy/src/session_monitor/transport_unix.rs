//! Unix-side transport for the per-session HTTP API. The bound `UnixListener` IS the sentinel
//! the consumer's filesystem watcher discovers (`{sessions_dir}/{session_id}.sock`), set to
//! mode `0o600` so only the owning user can connect.

#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use tokio::net::UnixListener;

/// Binds the per-session unix socket and returns it along with a [`SessionTransportCleanup`]
/// that removes the socket file on drop.
pub fn bind_session_transport(
    sessions_dir: &Path,
    session_id: &str,
) -> std::io::Result<(UnixListener, SessionTransportCleanup)> {
    let socket_path = sessions_dir.join(format!("{session_id}.sock"));

    if let Err(err) = fs::remove_file(&socket_path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(?err, ?socket_path, "Failed to remove stale session socket");
    }

    let listener = UnixListener::bind(&socket_path)?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;

    Ok((listener, SessionTransportCleanup { path: socket_path }))
}

/// Removes the session socket file on drop. Without this, killed sessions would leave stale
/// `.sock` entries in `~/.mirrord/sessions` that the consumer would then try (and fail) to
/// connect to before treating them as removable garbage.
pub struct SessionTransportCleanup {
    path: PathBuf,
}

impl Drop for SessionTransportCleanup {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            tracing::warn!(?err, path = ?self.path, "Failed to remove session socket");
        }
    }
}
