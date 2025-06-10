use std::sync::Arc;

use async_trait::async_trait;
use tracing::Level;

use crate::{
    chain::IPTableChain, error::IPTablesResult, redirect::Redirect, IPTables,
    IPTABLE_EXCLUDE_FROM_MESH,
};

/// Type used for excluding certain ports from the service mesh proxy.
#[derive(Debug)]
pub struct MeshExclusion<IPT: IPTables> {
    managed: IPTableChain<IPT>,
}

impl<IPT> MeshExclusion<IPT>
where
    IPT: IPTables,
{
    const ENTRYPOINT: &'static str = "PREROUTING";

    /// Create a new `chain` and mount it.
    pub fn create(ipt: Arc<IPT>, chain: &str) -> IPTablesResult<Self> {
        let managed = IPTableChain::create(ipt, chain.to_string())?;

        Ok(Self { managed })
    }

    /// Load existing `chain` and mount it.
    pub fn load(ipt: Arc<IPT>, chain: &str) -> IPTablesResult<Self> {
        let managed = IPTableChain::load(ipt, chain.to_string())?;

        Ok(Self { managed })
    }

    pub fn mount_entrypoint(&self) -> IPTablesResult<()> {
        self.managed.inner().insert_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.managed.chain_name()),
            1,
        )?;

        Ok(())
    }

    pub fn unmount_entrypoint(&self) -> IPTablesResult<()> {
        self.managed.inner().remove_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.managed.chain_name()),
        )?;

        Ok(())
    }

    pub fn add_exclusion(&self, port: u16) -> IPTablesResult<()> {
        self.managed.add_rule(Self::accept_port_rule(port))?;
        Ok(())
    }

    pub fn remove_exclusion(&self, port: u16) -> IPTablesResult<()> {
        self.managed.remove_rule(Self::accept_port_rule(port))?;
        Ok(())
    }

    fn accept_port_rule(port: u16) -> String {
        format!("-p tcp --dport {} -j ACCEPT", port)
    }
}

#[derive(Debug)]
pub struct WithMeshExclusion<IPT: IPTables, T> {
    exclusion: MeshExclusion<IPT>,
    inner: Box<T>,
}

impl<IPT, T> WithMeshExclusion<IPT, T>
where
    IPT: IPTables,
    T: Redirect,
{
    #[tracing::instrument(level = Level::TRACE, skip_all)]
    pub fn create(ipt: Arc<IPT>, inner: Box<T>) -> IPTablesResult<Self> {
        let exclusion = MeshExclusion::create(ipt, IPTABLE_EXCLUDE_FROM_MESH)?;

        Ok(WithMeshExclusion { exclusion, inner })
    }

    #[tracing::instrument(level = Level::TRACE, skip_all)]
    pub fn load(ipt: Arc<IPT>, inner: Box<T>) -> IPTablesResult<Self> {
        let exclusion = MeshExclusion::load(ipt, IPTABLE_EXCLUDE_FROM_MESH)?;

        Ok(WithMeshExclusion { exclusion, inner })
    }

    pub fn exclusion(&self) -> &MeshExclusion<IPT> {
        &self.exclusion
    }
}

#[async_trait]
impl<IPT, T> Redirect for WithMeshExclusion<IPT, T>
where
    IPT: IPTables + Send + Sync,
    T: Redirect + Send + Sync,
{
    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err)]
    async fn mount_entrypoint(&self) -> IPTablesResult<()> {
        // Should mount the internal iptables because exclusion inserts itself as first rule and
        // this may also happen here so exclusion should mount second
        self.inner.mount_entrypoint().await?;

        self.exclusion.mount_entrypoint()
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err)]
    async fn unmount_entrypoint(&self) -> IPTablesResult<()> {
        self.inner.unmount_entrypoint().await?;

        self.exclusion.unmount_entrypoint()
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err)]
    async fn add_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()> {
        self.inner.add_redirect(redirected_port, target_port).await
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err)]
    async fn remove_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()> {
        self.inner
            .remove_redirect(redirected_port, target_port)
            .await
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::eq;

    use super::*;
    use crate::MockIPTables;

    #[test]
    fn default() {
        let mut mock = MockIPTables::new();

        mock.expect_create_chain()
            .with(eq(IPTABLE_EXCLUDE_FROM_MESH))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq("PREROUTING"),
                eq(format!("-j {}", IPTABLE_EXCLUDE_FROM_MESH)),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_EXCLUDE_FROM_MESH),
                eq("-p tcp --dport 1337 -j ACCEPT"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(IPTABLE_EXCLUDE_FROM_MESH),
                eq("-p tcp --dport 1337 -j ACCEPT"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq("PREROUTING"),
                eq(format!("-j {}", IPTABLE_EXCLUDE_FROM_MESH)),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_EXCLUDE_FROM_MESH))
            .times(1)
            .returning(|_| Ok(()));

        let exclusion =
            MeshExclusion::create(mock.into(), IPTABLE_EXCLUDE_FROM_MESH).expect("Create Failed");

        assert!(exclusion.mount_entrypoint().is_ok());

        assert!(exclusion.add_exclusion(1337).is_ok());

        assert!(exclusion.remove_exclusion(1337).is_ok());

        assert!(exclusion.unmount_entrypoint().is_ok());
    }
}
