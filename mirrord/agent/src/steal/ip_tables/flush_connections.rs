use std::sync::Arc;

use async_trait::async_trait;
use mirrord_protocol::Port;
use tokio::process::Command;
use tracing::warn;

use crate::{
    error::Result,
    steal::ip_tables::{chain::IPTableChain, redirect::Redirect, IPTables, IPTABLE_INPUT},
};

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

    pub fn create(ipt: Arc<IPT>, inner: Box<T>) -> Result<Self> {
        let managed =
            IPTableChain::create(ipt.with_table("filter").into(), IPTABLE_INPUT.to_string())?;

        managed.add_rule("-p tcp -m state --state NEW,ESTABLISHED,RELATED -j ACCEPT")?;
        managed.add_rule("-p tcp -j DROP")?;

        Ok(FlushConnections { managed, inner })
    }

    pub fn load(ipt: Arc<IPT>, inner: Box<T>) -> Result<Self> {
        let managed =
            IPTableChain::create(ipt.with_table("filter").into(), IPTABLE_INPUT.to_string())?;

        Ok(FlushConnections { managed, inner })
    }
}

#[async_trait]
impl<IPT, T> Redirect for FlushConnections<IPT, T>
where
    IPT: IPTables + Send + Sync,
    T: Redirect + Send + Sync,
{
    async fn mount_entrypoint(&self) -> Result<()> {
        self.inner.mount_entrypoint().await?;

        self.managed.inner().add_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.managed.chain_name()),
        )?;

        Ok(())
    }

    async fn unmount_entrypoint(&self) -> Result<()> {
        self.inner.unmount_entrypoint().await?;

        self.managed.inner().remove_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.managed.chain_name()),
        )?;

        Ok(())
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
