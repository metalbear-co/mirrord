//! Flush connections - feature that enables the agent to steal connections that are in progress
//! What do you mean? Imagine there was an ongoing session between Client and Server, then the
//! agent starts, the layer asks to listen on the same port as the Server, and then the agent
//! will only get **the next connection** since the redirection happens on the nat table
//! which is hit only for new connections.
//! Flush connections overcomes this by marking all existing connections of a specific port,
//! and adding a rule that marked connections will be rejected.

use std::ops::Not;

use async_trait::async_trait;
use mirrord_agent_env::envs;
use tokio::process::Command;
use tracing::{Level, warn};

use crate::{error::IPTablesResult, redirect::Redirect};

/// Reads whether the flush should use `conntrack -D`.
///
/// Defaults to `true`; only an explicit `false` disables it. See
/// [`STEALER_FLUSH_CONNECTIONS_CONNTRACK`](envs::STEALER_FLUSH_CONNECTIONS_CONNTRACK).
fn conntrack_enabled() -> bool {
    envs::STEALER_FLUSH_CONNECTIONS_CONNTRACK
        .try_from_env()
        .ok()
        .flatten()
        .unwrap_or(true)
}

#[derive(Debug)]
pub struct FlushConnections<T> {
    inner: Box<T>,
    /// Whether to flush existing connections with a `conntrack -D` command in addition to `ss -K`.
    conntrack: bool,
}

impl<T> FlushConnections<T>
where
    T: Redirect,
{
    #[tracing::instrument(level = Level::TRACE, skip(inner))]
    pub fn create(inner: Box<T>) -> IPTablesResult<Self> {
        Ok(FlushConnections {
            inner,
            conntrack: conntrack_enabled(),
        })
    }

    #[tracing::instrument(level = Level::TRACE, skip(inner))]
    pub fn load(inner: Box<T>) -> IPTablesResult<Self> {
        Ok(FlushConnections {
            inner,
            conntrack: conntrack_enabled(),
        })
    }
}

/// Contained in `conntract -D` command stderr when there is no entry to drop.
const NO_ENTRIES_DELETED_MESSAGE: &[u8] = b"0 flow entries have been deleted";

#[async_trait]
impl<T> Redirect for FlushConnections<T>
where
    T: Redirect + Send + Sync,
{
    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err)]
    async fn mount_entrypoint(&self) -> IPTablesResult<()> {
        self.inner.mount_entrypoint().await
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err)]
    async fn unmount_entrypoint(&self) -> IPTablesResult<()> {
        self.inner.unmount_entrypoint().await
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err)]
    async fn add_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()> {
        self.inner
            .add_redirect(redirected_port, target_port)
            .await?;

        // Update existing connections of specific port to be marked
        // so that they will be rejected by the rule we added in `create`.
        if self.conntrack {
            let port = redirected_port.to_string();

            // `--dport` alone matches both connections that existed before the redirect rule and
            // connections the rule has just redirected to the agent.
            // The two only differ in the reply tuple: an untouched connection still replies from
            // `redirected_port`, while a redirected one replies from the agent's listener port.
            // Filtering on `--reply-port-src` therefore deletes only the former.
            let args = [
                "-D",
                "-p",
                "tcp",
                "--dport",
                &port,
                "--reply-port-src",
                &port,
                "--state",
                "ESTABLISHED",
            ];

            let conntrack_output = Command::new("conntrack").args(args).output().await?;

            tracing::trace!(
                redirected_port,
                status = ?conntrack_output.status,
                stderr = %String::from_utf8_lossy(&conntrack_output.stderr).trim(),
                "`conntrack -D` command finished",
            );

            if conntrack_output.status.success().not()
                && conntrack_output
                    .stderr
                    .windows(NO_ENTRIES_DELETED_MESSAGE.len())
                    .any(|window| window == NO_ENTRIES_DELETED_MESSAGE)
                    .not()
            {
                warn!(
                    ?conntrack_output,
                    redirected_port,
                    "Failed to flush existing connections with a `conntrack -D` command",
                );
            }
        }

        // ss is available from kernel 5.1 and should work way better than conntrack
        let formatted_port = format!(":{}", redirected_port);

        let ss_output = Command::new("ss")
            .args(["-K", "sport", "=", &formatted_port])
            .output()
            .await?;

        if ss_output.status.success().not() {
            warn!(
                ?ss_output,
                redirected_port, "Failed to flush existing connections with an `ss -K` command",
            );
        }

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err)]
    async fn remove_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()> {
        self.inner
            .remove_redirect(redirected_port, target_port)
            .await?;

        Ok(())
    }
}
