//! `axum::serve::Listener` implementation backed by a single named pipe address. Each accepted
//! connection is a fresh `NamedPipeServer` instance; the listener pre-allocates the next
//! instance after a connection comes in so subsequent client connects don't observe a
//! transient `ERROR_PIPE_NOT_FOUND`.
//!
//! Pipes are created with a DACL restricting access to the current user (see
//! [`super::win_security::PipeSecurity`]). This matches the `0o600` restriction we apply to
//! the unix socket.

#![cfg(windows)]

use std::{io, time::Duration};

use axum::serve::Listener;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use super::win_security::PipeSecurity;

pub struct NamedPipeListener {
    pipe_name: String,
    next: Option<NamedPipeServer>,
    sa: PipeSecurity,
}

impl NamedPipeListener {
    pub fn bind(pipe_name: String) -> io::Result<Self> {
        let sa = PipeSecurity::for_current_user()?;
        let next = create_instance(&pipe_name, &sa, true)?;
        Ok(Self {
            pipe_name,
            next: Some(next),
            sa,
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
                None => match create_instance(&self.pipe_name, &self.sa, false) {
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
            self.next = create_instance(&self.pipe_name, &self.sa, false).ok();
            return (server, ());
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(())
    }
}

fn create_instance(
    pipe_name: &str,
    sa: &PipeSecurity,
    first_instance: bool,
) -> io::Result<NamedPipeServer> {
    let mut opts = ServerOptions::new();
    if first_instance {
        opts.first_pipe_instance(true);
    }
    // SAFETY: `sa.as_raw()` returns a pointer to a `SECURITY_ATTRIBUTES` whose backing
    // buffers are owned by `sa`, which outlives this call. The kernel copies the descriptor
    // into the pipe handle synchronously, so the buffers don't need to live past return.
    unsafe { opts.create_with_security_attributes_raw(pipe_name, sa.as_raw()) }
}
