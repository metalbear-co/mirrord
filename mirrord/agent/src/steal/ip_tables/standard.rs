use std::sync::Arc;

use async_trait::async_trait;
use mirrord_protocol::Port;
use nix::unistd::getgid;

use crate::{
    error::Result,
    steal::ip_tables::{
        chain::IPTableChain, redirect::PreroutingRedirect, IPTables, Redirect, IPTABLE_STANDARD,
    },
};

pub struct StandardRedirect<IPT: IPTables> {
    preroute: PreroutingRedirect<IPT>,
    managed: IPTableChain<IPT>,
    own_packet_filter: String,
}

impl<IPT> StandardRedirect<IPT>
where
    IPT: IPTables,
{
    const ENTRYPOINT: &'static str = "OUTPUT";

    pub fn create(ipt: Arc<IPT>) -> Result<Self> {
        let own_packet_filter = Self::get_own_packet_filter(&ipt)?;
        let preroute = PreroutingRedirect::create(ipt.clone())?;
        let managed = IPTableChain::create(ipt, IPTABLE_STANDARD.to_string())?;

        let gid = getgid();
        managed.add_rule(&format!("-m owner --gid-owner {gid} -p tcp -j RETURN"))?;

        Ok(StandardRedirect {
            preroute,
            managed,
            own_packet_filter,
        })
    }

    pub fn load(ipt: Arc<IPT>) -> Result<Self> {
        let own_packet_filter = Self::get_own_packet_filter(&ipt)?;
        let preroute = PreroutingRedirect::load(ipt.clone())?;
        let managed = IPTableChain::create(ipt, IPTABLE_STANDARD.to_string())?;

        Ok(StandardRedirect {
            preroute,
            managed,
            own_packet_filter,
        })
    }

    fn get_own_packet_filter(ipt: &IPT) -> Result<String> {
        let rules = ipt.list_rules(&IPTABLE_STANDARD)?;

        let own_packet_filter = if rules.is_empty() {
            "-o lo".to_owned()
        } else {
            rules
                .iter()
                .map(|m| format!("-o lo {}", m.as_str()))
                .collect()
        };

        Ok(own_packet_filter)
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
            &format!("-j {}", self.managed.chain_name()),
        )?;

        Ok(())
    }

    async fn unmount_entrypoint(&self) -> Result<()> {
        self.preroute.unmount_entrypoint().await?;

        self.managed.inner().remove_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.managed.chain_name()),
        )?;

        Ok(())
    }

    async fn add_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.preroute
            .add_redirect(redirected_port, target_port)
            .await?;
        let redirect_rule =
            format!("-m tcp -p tcp --dport {redirected_port} -j REDIRECT --to-ports {target_port}");

        self.managed.add_rule(&redirect_rule)?;

        Ok(())
    }

    async fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.preroute
            .remove_redirect(redirected_port, target_port)
            .await?;

        let redirect_rule =
            format!("-m tcp -p tcp --dport {redirected_port} -j REDIRECT --to-ports {target_port}");

        self.managed.remove_rule(&redirect_rule)?;

        Ok(())
    }
}
