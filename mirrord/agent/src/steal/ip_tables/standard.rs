use std::sync::Arc;

use async_trait::async_trait;
use mirrord_protocol::Port;

use crate::{
    error::Result,
    steal::ip_tables::{
        chain::IPTableChain, redirect::PreroutingRedirect, IPTables, Redirect, IPTABLE_STANDARD,
    },
};

pub struct StandardRedirect<IPT: IPTables> {
    preroute: PreroutingRedirect<IPT>,
    managed: IPTableChain<IPT>,
}

impl<IPT> StandardRedirect<IPT>
where
    IPT: IPTables,
{
    const ENTRYPOINT: &'static str = "OUTPUT";

    pub fn create(ipt: Arc<IPT>) -> Result<Self> {
        let preroute = PreroutingRedirect::create(ipt.clone())?;
        let managed = IPTableChain::create(ipt, IPTABLE_STANDARD.to_string())?;

        Ok(StandardRedirect { preroute, managed })
    }

    pub fn load(ipt: Arc<IPT>) -> Result<Self> {
        let preroute = PreroutingRedirect::load(ipt.clone())?;
        let managed = IPTableChain::create(ipt, IPTABLE_STANDARD.to_string())?;

        Ok(StandardRedirect { preroute, managed })
    }
}

/// This wrapper adds a new rule to the NAT OUTPUT chain to redirect "localhost" traffic as well
/// Note: OUTPUT chain is only traversed for packets produced by local applications
#[async_trait]
impl<IPT> Redirect for StandardRedirect<IPT>
where
    IPT: IPTables + Send + Sync,
{
    async fn mount_entrypoint(&self) -> Result<()> {
        self.preroute.mount_entrypoint().await?;

        self.managed.inner().add_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.preroute.managed.chain_name()),
        )?;

        Ok(())
    }

    async fn unmount_entrypoint(&self) -> Result<()> {
        self.preroute.unmount_entrypoint().await?;

        self.managed.inner().remove_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.preroute.managed.chain_name()),
        )?;

        Ok(())
    }

    async fn add_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        let redirect_rule =
            format!("-m tcp -p tcp --dport {redirected_port} -j REDIRECT --to-ports {target_port}");

        self.managed.add_rule(&redirect_rule)?;

        Ok(())
    }

    async fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        let redirect_rule =
            format!("-m tcp -p tcp --dport {redirected_port} -j REDIRECT --to-ports {target_port}");

        self.managed.remove_rule(&redirect_rule)?;

        Ok(())
    }
}
