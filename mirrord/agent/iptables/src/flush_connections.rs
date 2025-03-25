//! Flush connections - feature that enables the agent to steal connections that are in progress
//! What do you mean? Imagine there was an ongoing session between Client and Server, then the
//! agent starts, the layer asks to listen on the same port as the Server, and then the agent
//! will only get **the next connection** since the redirection happens on the nat table
//! which is hit only for new connections.
//! Flush connections overcomes this by marking all existing connections of a specific port,
//! and adding a rule that marked connections will be rejected.

use async_trait::async_trait;
use tokio::process::Command;
use tracing::{warn, Level};

use crate::{error::IPTablesResult, redirect::Redirect};

#[derive(Debug)]
pub struct FlushConnections<T> {
    inner: Box<T>,
}

impl<T> FlushConnections<T>
where
    T: Redirect,
{
    #[tracing::instrument(level = Level::TRACE, skip(inner))]
    pub fn create(inner: Box<T>) -> IPTablesResult<Self> {
        Ok(FlushConnections { inner })
    }

    #[tracing::instrument(level = Level::TRACE, skip(inner))]
    pub fn load(inner: Box<T>) -> IPTablesResult<Self> {
        Ok(FlushConnections { inner })
    }
}

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
        // so that they will be rejected by the rule we added in `create`
        let conntrack = Command::new("conntrack")
            .args([
                "-D",
                "-p",
                "tcp",
                "--dport",
                &redirected_port.to_string(),
                "--state",
                "ESTABLISHED",
            ])
            .output()
            .await?;

        if !conntrack.status.success() && conntrack.status.code() != Some(256) {
            warn!("`conntrack` output is {conntrack:#?}");
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
