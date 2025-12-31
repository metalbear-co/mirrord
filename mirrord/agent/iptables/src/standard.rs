use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    IPTABLE_STANDARD, IPTables, Redirect, error::IPTablesResult, output::OutputRedirect,
    prerouting::PreroutingRedirect,
};

pub struct StandardRedirect<IPT: IPTables> {
    prerouting: PreroutingRedirect<IPT>,
    output: OutputRedirect<false, IPT>,
}

impl<IPT> StandardRedirect<IPT>
where
    IPT: IPTables,
{
    pub fn create(ipt: Arc<IPT>, pod_ips: Option<&str>) -> IPTablesResult<Self> {
        let prerouting = PreroutingRedirect::create(ipt.clone())?;
        let output = OutputRedirect::create(ipt, IPTABLE_STANDARD.to_string(), pod_ips)?;

        Ok(StandardRedirect { prerouting, output })
    }

    pub fn load(ipt: Arc<IPT>) -> IPTablesResult<Self> {
        let prerouting = PreroutingRedirect::load(ipt.clone())?;
        let output = OutputRedirect::load(ipt, IPTABLE_STANDARD.to_string())?;

        Ok(StandardRedirect { prerouting, output })
    }
}

/// This wrapper adds a new rule to the NAT OUTPUT chain to redirect "localhost" traffic as well
/// Note: OUTPUT chain is only traversed for packets produced by local applications
#[async_trait]
impl<IPT> Redirect for StandardRedirect<IPT>
where
    IPT: IPTables + Send + Sync,
{
    async fn mount_entrypoint(&self) -> IPTablesResult<()> {
        self.prerouting.mount_entrypoint().await?;
        self.output.mount_entrypoint().await?;

        Ok(())
    }

    async fn unmount_entrypoint(&self) -> IPTablesResult<()> {
        // Don't fail early, so that we delete the second part even if the second part does not
        // exist and its deletion therefore fails.
        let prerouting_res = self.prerouting.unmount_entrypoint().await;
        let output_res = self.output.unmount_entrypoint().await;
        prerouting_res.and(output_res)
    }

    async fn add_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()> {
        self.prerouting
            .add_redirect(redirected_port, target_port)
            .await?;
        self.output
            .add_redirect(redirected_port, target_port)
            .await?;

        Ok(())
    }

    async fn remove_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()> {
        // Don't fail early, so that we delete the second part even if the second part does not
        // exist and its deletion therefore fails.
        let prerouting_res = self
            .prerouting
            .remove_redirect(redirected_port, target_port)
            .await;
        let output_res = self
            .output
            .remove_redirect(redirected_port, target_port)
            .await;
        prerouting_res.and(output_res)
    }
}
