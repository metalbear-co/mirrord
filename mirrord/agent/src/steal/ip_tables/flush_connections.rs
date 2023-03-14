use async_trait::async_trait;
use mirrord_protocol::Port;
use tokio::process::Command;
use tracing::warn;

use crate::{error::Result, steal::ip_tables::redirect::AsyncRedirect};

#[derive(Debug)]
pub struct FlushConnections<T> {
    inner: Box<T>,
}

impl<T> FlushConnections<T>
where
    T: AsyncRedirect,
{
    pub fn new(inner: Box<T>) -> Self {
        FlushConnections { inner }
    }

    pub fn inner(&self) -> &T {
        &self.inner
    }
}

#[async_trait]
impl<T> AsyncRedirect for FlushConnections<T>
where
    T: AsyncRedirect + Send + Sync,
{
    async fn async_mount_entrypoint(&self) -> Result<()> {
        self.inner.async_mount_entrypoint().await
    }

    async fn async_add_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.inner
            .async_add_redirect(redirected_port, target_port)
            .await?;

        let conntrack = Command::new("conntrack")
            .args([
                "--delete",
                "--proto",
                "tcp",
                "--orig-port-dst",
                &target_port.to_string(),
            ])
            .output()
            .await?;

        if !conntrack.status.success() && conntrack.status.code() != Some(256) {
            warn!("`conntrack` output is {conntrack:#?}");
        }

        Ok(())
    }

    async fn async_remove_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.inner
            .async_add_redirect(redirected_port, target_port)
            .await?;

        Ok(())
    }
}
