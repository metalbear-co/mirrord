use std::sync::Arc;

use async_trait::async_trait;
use mirrord_protocol::Port;

use crate::{
    error::Result,
    steal::ip_tables::{
        output::OutputRedirect, prerouting::PreroutingRedirect, IPTables, Redirect,
        IPTABLE_STANDARD,
    },
};

pub(crate) struct StandardRedirect<IPT: IPTables> {
    prerouteing: PreroutingRedirect<IPT>,
    output: OutputRedirect<IPT>,
}

impl<IPT> StandardRedirect<IPT>
where
    IPT: IPTables,
{
    pub fn create(ipt: Arc<IPT>, pod_ips: Option<&str>) -> Result<Self> {
        let prerouteing = PreroutingRedirect::create(ipt.clone())?;
        let output = OutputRedirect::create(ipt, IPTABLE_STANDARD.to_string(), pod_ips)?;

        Ok(StandardRedirect {
            prerouteing,
            output,
        })
    }

    pub fn load(ipt: Arc<IPT>) -> Result<Self> {
        let prerouteing = PreroutingRedirect::load(ipt.clone())?;
        let output = OutputRedirect::load(ipt, IPTABLE_STANDARD.to_string())?;

        Ok(StandardRedirect {
            prerouteing,
            output,
        })
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
        self.prerouteing.mount_entrypoint().await?;
        self.output.mount_entrypoint().await?;

        Ok(())
    }

    async fn unmount_entrypoint(&self) -> Result<()> {
        self.prerouteing.unmount_entrypoint().await?;
        self.output.unmount_entrypoint().await?;

        Ok(())
    }

    async fn add_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.prerouteing
            .add_redirect(redirected_port, target_port)
            .await?;
        self.output
            .add_redirect(redirected_port, target_port)
            .await?;

        Ok(())
    }

    async fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.prerouteing
            .remove_redirect(redirected_port, target_port)
            .await?;
        self.output
            .remove_redirect(redirected_port, target_port)
            .await?;

        Ok(())
    }
}
