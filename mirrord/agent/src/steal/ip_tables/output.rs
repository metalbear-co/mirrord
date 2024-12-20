use std::sync::Arc;

use async_trait::async_trait;
use mirrord_protocol::Port;
use nix::unistd::getgid;
use tracing::warn;

use crate::{
    error::AgentResult,
    steal::ip_tables::{chain::IPTableChain, IPTables, Redirect},
};

pub(crate) struct OutputRedirect<const USE_INSERT: bool, IPT: IPTables> {
    pub(crate) managed: IPTableChain<IPT>,
}

impl<const USE_INSERT: bool, IPT> OutputRedirect<USE_INSERT, IPT>
where
    IPT: IPTables,
{
    const ENTRYPOINT: &'static str = "OUTPUT";

    pub fn create(ipt: Arc<IPT>, chain_name: String, pod_ips: Option<&str>) -> AgentResult<Self> {
        let managed = IPTableChain::create(ipt, chain_name)?;

        let exclude_source_ips = pod_ips
            .map(|pod_ips| format!("! -s {pod_ips}"))
            .unwrap_or_default();

        let gid = getgid();
        managed
            .add_rule(&format!(
                "-m owner --gid-owner {gid} -p tcp {exclude_source_ips} -j RETURN"
            ))
            .inspect_err(|_| {
                warn!("Unable to create iptable rule with \"--gid-owner {gid}\" filter")
            })?;

        Ok(OutputRedirect { managed })
    }

    pub fn load(ipt: Arc<IPT>, chain_name: String) -> AgentResult<Self> {
        let managed = IPTableChain::load(ipt, chain_name)?;

        Ok(OutputRedirect { managed })
    }
}

/// This wrapper adds a new rule to the NAT OUTPUT chain to redirect "localhost" traffic as well
/// Note: OUTPUT chain is only traversed for packets produced by local applications
#[async_trait]
impl<const USE_INSERT: bool, IPT> Redirect for OutputRedirect<USE_INSERT, IPT>
where
    IPT: IPTables + Send + Sync,
{
    async fn mount_entrypoint(&self) -> AgentResult<()> {
        if USE_INSERT {
            self.managed.inner().insert_rule(
                Self::ENTRYPOINT,
                &format!("-j {}", self.managed.chain_name()),
                1,
            )?;
        } else {
            self.managed.inner().add_rule(
                Self::ENTRYPOINT,
                &format!("-j {}", self.managed.chain_name()),
            )?;
        }

        Ok(())
    }

    async fn unmount_entrypoint(&self) -> AgentResult<()> {
        self.managed.inner().remove_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.managed.chain_name()),
        )?;

        Ok(())
    }

    async fn add_redirect(&self, redirected_port: Port, target_port: Port) -> AgentResult<()> {
        let redirect_rule = format!(
            "-o lo -m tcp -p tcp --dport {redirected_port} -j REDIRECT --to-ports {target_port}"
        );

        self.managed.add_rule(&redirect_rule)?;

        Ok(())
    }

    async fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> AgentResult<()> {
        let redirect_rule = format!(
            "-o lo -m tcp -p tcp --dport {redirected_port} -j REDIRECT --to-ports {target_port}"
        );

        self.managed.remove_rule(&redirect_rule)?;

        Ok(())
    }
}
