use async_trait::async_trait;
use mirrord_protocol::Port;
use tokio::process::Command;
use tracing::warn;

use crate::{error::Result, steal::ip_tables::redirect::Redirect};

#[derive(Debug)]
pub struct FlushConnections<T> {
    inner: Box<T>,
}

impl<T> FlushConnections<T>
where
    T: Redirect,
{
    pub fn new(inner: Box<T>) -> Self {
        FlushConnections { inner }
    }
}

#[async_trait]
impl<T> Redirect for FlushConnections<T>
where
    T: Redirect + Send + Sync,
{
    async fn mount_entrypoint(&self) -> Result<()> {
        self.inner.mount_entrypoint().await
    }

    async fn unmount_entrypoint(&self) -> Result<()> {
        self.inner.unmount_entrypoint().await
    }

    async fn add_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.inner
            .add_redirect(redirected_port, target_port)
            .await?;

        let conntrack = Command::new("conntrack")
            .args([
                "--delete",
                "--proto",
                "tcp",
                "--orig-port-dst",
                &redirected_port.to_string(),
            ])
            .output()
            .await?;

        if !conntrack.status.success() && conntrack.status.code() != Some(256) {
            warn!("`conntrack` output is {conntrack:#?}");
        }

        Ok(())
    }

    async fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.inner
            .remove_redirect(redirected_port, target_port)
            .await?;

        Ok(())
    }
}
