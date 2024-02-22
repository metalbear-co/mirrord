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
    pub fn create(ipt: Arc<IPT>) -> Result<Self> {
        let prerouteing = PreroutingRedirect::create(ipt.clone())?;
        let output = OutputRedirect::create(ipt, IPTABLE_STANDARD.to_string())?;

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
        tokio::try_join!(
            self.prerouteing.mount_entrypoint(),
            self.output.mount_entrypoint(),
        )?;

        Ok(())
    }

    async fn unmount_entrypoint(&self) -> Result<()> {
        tokio::try_join!(
            self.prerouteing.unmount_entrypoint(),
            self.output.unmount_entrypoint(),
        )?;

        Ok(())
    }

    async fn add_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        tokio::try_join!(
            self.prerouteing.add_redirect(redirected_port, target_port),
            self.output.add_redirect(redirected_port, target_port)
        )?;

        Ok(())
    }

    async fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        tokio::try_join!(
            self.prerouteing
                .remove_redirect(redirected_port, target_port),
            self.output.remove_redirect(redirected_port, target_port),
        )?;

        Ok(())
    }
}
