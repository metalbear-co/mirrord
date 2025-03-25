use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    error::IPTablesResult, output::OutputRedirect, prerouting::PreroutingRedirect,
    redirect::Redirect, IPTables, IPTABLE_IPV4_ROUTE_LOCALNET_ORIGINAL, IPTABLE_MESH,
};

pub struct AmbientRedirect<IPT: IPTables> {
    prerouting: PreroutingRedirect<IPT>,
    output: OutputRedirect<true, IPT>,
}

impl<IPT> AmbientRedirect<IPT>
where
    IPT: IPTables,
{
    pub fn create(ipt: Arc<IPT>, pod_ips: Option<&str>) -> IPTablesResult<Self> {
        let prerouting = PreroutingRedirect::create(ipt.clone())?;
        let output = OutputRedirect::create(ipt, IPTABLE_MESH.to_string(), pod_ips)?;

        Ok(AmbientRedirect { prerouting, output })
    }

    pub fn load(ipt: Arc<IPT>) -> IPTablesResult<Self> {
        let prerouting = PreroutingRedirect::load(ipt.clone())?;
        let output = OutputRedirect::load(ipt, IPTABLE_MESH.to_string())?;

        Ok(AmbientRedirect { prerouting, output })
    }
}

#[async_trait]
impl<IPT> Redirect for AmbientRedirect<IPT>
where
    IPT: IPTables + Send + Sync,
{
    async fn mount_entrypoint(&self) -> IPTablesResult<()> {
        tokio::fs::write("/proc/sys/net/ipv4/conf/all/route_localnet", "1".as_bytes()).await?;

        self.prerouting.mount_entrypoint().await?;
        self.output.mount_entrypoint().await?;

        Ok(())
    }

    async fn unmount_entrypoint(&self) -> IPTablesResult<()> {
        self.prerouting.unmount_entrypoint().await?;
        self.output.unmount_entrypoint().await?;

        tokio::fs::write(
            "/proc/sys/net/ipv4/conf/all/route_localnet",
            IPTABLE_IPV4_ROUTE_LOCALNET_ORIGINAL.as_bytes(),
        )
        .await?;

        Ok(())
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
        self.prerouting
            .remove_redirect(redirected_port, target_port)
            .await?;
        self.output
            .remove_redirect(redirected_port, target_port)
            .await?;

        Ok(())
    }
}
