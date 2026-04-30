//! Windows-side transport for the per-session HTTP API. Backed by a single named pipe
//! address; each accepted connection is a fresh `NamedPipeServer` instance and the listener
//! pre-allocates the next instance after a connection comes in so subsequent client connects
//! don't observe a transient `ERROR_PIPE_NOT_FOUND`.
//!
//! Pipes are created with a DACL restricting access to the current user (see
//! [`super::win_security::PipeSecurity`]), matching the `0o600` restriction on the unix
//! socket. A zero-byte sentinel file at `{sessions_dir}/{session_id}.pipe` lets the
//! filesystem-watcher-based discovery on the consumer side work the same way as on unix.

#![cfg(windows)]

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use axum::serve::Listener;
use mirrord_session_monitor_protocol::pipe_name_for_session;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use super::win_security::PipeSecurity;

/// Writes the session sentinel marker file and binds the named pipe with a current-user-only
/// DACL. Returns the listener and a [`SessionTransportCleanup`] guard that removes the
/// sentinel on drop.
pub fn bind_session_transport(
    sessions_dir: &Path,
    session_id: &str,
) -> io::Result<(NamedPipeListener, SessionTransportCleanup)> {
    let sentinel_path = sessions_dir.join(format!("{session_id}.pipe"));
    let pipe_name = pipe_name_for_session(session_id);

    fs::write(&sentinel_path, b"")?;

    let listener = NamedPipeListener::bind(pipe_name)?;

    Ok((
        listener,
        SessionTransportCleanup {
            path: sentinel_path,
        },
    ))
}

/// Removes the session sentinel file on drop. The named pipe handle is closed automatically
/// when [`NamedPipeListener`] drops; only the marker file persists in the filesystem and
/// needs explicit cleanup.
pub struct SessionTransportCleanup {
    path: PathBuf,
}

impl Drop for SessionTransportCleanup {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            tracing::warn!(?err, path = ?self.path, "Failed to remove session sentinel");
        }
    }
}

pub struct NamedPipeListener {
    pipe_name: String,
    next: Option<NamedPipeServer>,
    security_attributes: PipeSecurity,
}

impl NamedPipeListener {
    pub fn bind(pipe_name: String) -> io::Result<Self> {
        let security_attributes = PipeSecurity::for_current_user()?;
        let next = create_instance(&pipe_name, &security_attributes, true)?;
        Ok(Self {
            pipe_name,
            next: Some(next),
            security_attributes,
        })
    }
}

impl Listener for NamedPipeListener {
    type Io = NamedPipeServer;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let server = match self.next.take() {
                Some(server) => server,
                None => match create_instance(&self.pipe_name, &self.security_attributes, false) {
                    Ok(server) => server,
                    Err(err) => {
                        tracing::warn!(
                            ?err,
                            pipe = self.pipe_name,
                            "failed to create named pipe instance, retrying"
                        );
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                },
            };

            if let Err(err) = server.connect().await {
                tracing::warn!(
                    ?err,
                    pipe = self.pipe_name,
                    "named pipe connect failed, retrying"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            // Pre-create the next instance so the immediate next client doesn't get
            // `ERROR_PIPE_NOT_FOUND`. If creation fails here, we'll lazily try again on the
            // next `accept`.
            self.next = create_instance(&self.pipe_name, &self.security_attributes, false).ok();
            return (server, ());
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(())
    }
}

fn create_instance(
    pipe_name: &str,
    security_attributes: &PipeSecurity,
    first_instance: bool,
) -> io::Result<NamedPipeServer> {
    let mut opts = ServerOptions::new();
    if first_instance {
        opts.first_pipe_instance(true);
    }
    // SAFETY: `security_attributes.as_raw()` returns a pointer to a `SECURITY_ATTRIBUTES`
    // whose backing buffers are owned by `security_attributes`, which outlives this call.
    // The kernel copies the descriptor into the pipe handle synchronously, so the buffers
    // don't need to live past return.
    unsafe { opts.create_with_security_attributes_raw(pipe_name, security_attributes.as_raw()) }
}
