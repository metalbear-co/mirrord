//! Flush connections - feature that enables the agent to steal connections that are in progress
//! What do you mean? Imagine there was an ongoing session between Client and Server, then the
//! agent starts, the layer asks to listen on the same port as the Server, and then the agent
//! will only get **the next connection** since the redirection happens on the nat table
//! which is hit only for new connections.
//! Flush connections overcomes this by marking all existing connections of a specific port,
//! and adding a rule that marked connections will be rejected.
use std::sync::Arc;

use async_trait::async_trait;
use mirrord_protocol::Port;
use tokio::process::Command;
use tracing::warn;

use crate::{
    error::Result,
    steal::ip_tables::{chain::IPTableChain, redirect::Redirect, IPTables, IPTABLE_INPUT},
};

const MARK: &str = "0x1";

#[derive(Debug)]
pub struct FlushConnections<IPT: IPTables, T> {
    managed: IPTableChain<IPT>,
    inner: Box<T>,
}

impl<IPT, T> FlushConnections<IPT, T>
where
    IPT: IPTables,
    T: Redirect,
{
    const ENTRYPOINT: &'static str = "INPUT";

    #[tracing::instrument(level = "trace", skip(ipt, inner))]
    pub fn create(ipt: Arc<IPT>, inner: Box<T>) -> Result<Self> {
        let managed =
            IPTableChain::create(ipt.with_table("filter").into(), IPTABLE_INPUT.to_string())?;

        // specify tcp protocol, if we don't we can't reject with tcp-reset
        managed.add_rule(&format!(
            "-p tcp -m connmark --mark {MARK} -j REJECT --reject-with tcp-reset"
        ))?;

        Ok(FlushConnections { managed, inner })
    }

    #[tracing::instrument(level = "trace", skip(ipt, inner))]
    pub fn load(ipt: Arc<IPT>, inner: Box<T>) -> Result<Self> {
        let managed =
            IPTableChain::load(ipt.with_table("filter").into(), IPTABLE_INPUT.to_string())?;

        Ok(FlushConnections { managed, inner })
    }
}

#[async_trait]
impl<IPT, T> Redirect for FlushConnections<IPT, T>
where
    IPT: IPTables + Send + Sync,
    T: Redirect + Send + Sync,
{
    #[tracing::instrument(level = "trace", skip(self), ret)]
    async fn mount_entrypoint(&self) -> Result<()> {
        self.inner.mount_entrypoint().await?;

        self.managed.inner().add_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.managed.chain_name()),
        )?;

        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self), ret)]
    async fn unmount_entrypoint(&self) -> Result<()> {
        self.inner.unmount_entrypoint().await?;

        self.managed.inner().remove_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.managed.chain_name()),
        )?;

        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self), ret)]
    async fn add_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.inner
            .add_redirect(redirected_port, target_port)
            .await?;

        // Update existing connections of specific port to be marked
        // so that they will be rejected by the rule we added in `create`
        let conntrack = Command::new("conntrack")
            .args([
                "-U",
                "-p",
                "tcp",
                "-dport",
                &redirected_port.to_string(),
                "-m",
                MARK,
            ])
            .output()
            .await?;

        if !conntrack.status.success() && conntrack.status.code() != Some(256) {
            warn!("`conntrack` output is {conntrack:#?}");
        }

        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self), ret)]
    async fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.inner
            .remove_redirect(redirected_port, target_port)
            .await?;

        Ok(())
    }
}
