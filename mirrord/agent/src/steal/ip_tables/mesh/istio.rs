use std::sync::Arc;

use async_trait::async_trait;
use mirrord_protocol::Port;

use crate::{
    error::Result,
    steal::ip_tables::{
        output::OutputRedirect, prerouting::PreroutingRedirect, redirect::Redirect, IPTables,
        IPTABLE_IPV4_ROUTE_LOCALNET_ORIGINAL, IPTABLE_MESH,
    },
};

pub(crate) struct AmbientRedirect<IPT: IPTables> {
    prerouteing: PreroutingRedirect<IPT>,
    output: OutputRedirect<true, IPT>,
}

impl<IPT> AmbientRedirect<IPT>
where
    IPT: IPTables,
{
    pub fn create(ipt: Arc<IPT>, pod_ips: Option<&str>) -> Result<Self> {
        let prerouteing = PreroutingRedirect::create(ipt.clone())?;
        let output = OutputRedirect::create(ipt, IPTABLE_MESH.to_string(), pod_ips)?;

        Ok(AmbientRedirect {
            prerouteing,
            output,
        })
    }

    pub fn load(ipt: Arc<IPT>) -> Result<Self> {
        let prerouteing = PreroutingRedirect::load(ipt.clone())?;
        let output = OutputRedirect::load(ipt, IPTABLE_MESH.to_string())?;

        Ok(AmbientRedirect {
            prerouteing,
            output,
        })
    }
}

#[async_trait]
impl<IPT> Redirect for AmbientRedirect<IPT>
where
    IPT: IPTables + Send + Sync,
{
    async fn mount_entrypoint(&self) -> Result<()> {
        tokio::fs::write("/proc/sys/net/ipv4/conf/all/route_localnet", "1".as_bytes()).await?;

        self.prerouteing.mount_entrypoint().await?;
        self.output.mount_entrypoint().await?;

        Ok(())
    }

    async fn unmount_entrypoint(&self) -> Result<()> {
        self.prerouteing.unmount_entrypoint().await?;
        self.output.unmount_entrypoint().await?;

        tokio::fs::write(
            "/proc/sys/net/ipv4/conf/all/route_localnet",
            IPTABLE_IPV4_ROUTE_LOCALNET_ORIGINAL.as_bytes(),
        )
        .await?;

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
