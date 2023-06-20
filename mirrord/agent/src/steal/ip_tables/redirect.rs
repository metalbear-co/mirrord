use std::{ops::Deref, sync::Arc};

use async_trait::async_trait;
use enum_dispatch::enum_dispatch;
use mirrord_protocol::Port;

use crate::{
    error::Result,
    steal::ip_tables::{chain::IPTableChain, IPTables, IPTABLE_PREROUTING},
};

#[async_trait]
#[enum_dispatch]
pub trait Redirect {
    async fn mount_entrypoint(&self) -> Result<()>;

    async fn unmount_entrypoint(&self) -> Result<()>;

    /// Create port redirection
    async fn add_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()>;
    /// Remove port redirection
    async fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()>;
}

pub struct PreroutingRedirect<IPT: IPTables> {
    managed: IPTableChain<IPT>,
}

impl<IPT> PreroutingRedirect<IPT>
where
    IPT: IPTables,
{
    const ENTRYPOINT: &'static str = "PREROUTING";

    pub fn create(ipt: Arc<IPT>) -> Result<Self> {
        let managed = IPTableChain::create(ipt, IPTABLE_PREROUTING.to_string())?;

        Ok(PreroutingRedirect { managed })
    }

    pub fn load(ipt: Arc<IPT>) -> Result<Self> {
        let managed = IPTableChain::load(ipt, IPTABLE_PREROUTING.to_string())?;

        Ok(PreroutingRedirect { managed })
    }
}

#[async_trait]
impl<IPT> Redirect for PreroutingRedirect<IPT>
where
    IPT: IPTables + Send + Sync,
{
    async fn mount_entrypoint(&self) -> Result<()> {
        self.managed.inner().add_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.managed.chain_name()),
        )?;

        Ok(())
    }

    async fn unmount_entrypoint(&self) -> Result<()> {
        self.managed.inner().remove_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.managed.chain_name()),
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

impl<IPT> Deref for PreroutingRedirect<IPT>
where
    IPT: IPTables,
{
    type Target = IPTableChain<IPT>;

    fn deref(&self) -> &Self::Target {
        &self.managed
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::*;

    use super::*;
    use crate::steal::ip_tables::MockIPTables;

    #[tokio::test]
    async fn add_redirect() {
        let mut mock = MockIPTables::new();

        mock.expect_create_chain()
            .with(eq(IPTABLE_PREROUTING.as_str()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_PREROUTING.as_str()),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_PREROUTING.as_str()))
            .times(1)
            .returning(|_| Ok(()));

        let prerouting = PreroutingRedirect::create(Arc::new(mock)).expect("Unable to create");

        assert!(prerouting.add_redirect(69, 420).await.is_ok());
    }

    #[tokio::test]
    async fn add_redirect_twice() {
        let mut mock = MockIPTables::new();

        mock.expect_create_chain()
            .with(eq(IPTABLE_PREROUTING.as_str()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_PREROUTING.as_str()),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_PREROUTING.as_str()),
                eq("-m tcp -p tcp --dport 169 -j REDIRECT --to-ports 1420"),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_PREROUTING.as_str()))
            .times(1)
            .returning(|_| Ok(()));

        let prerouting = PreroutingRedirect::create(Arc::new(mock)).expect("Unable to create");

        assert!(prerouting.add_redirect(69, 420).await.is_ok());
        assert!(prerouting.add_redirect(169, 1420).await.is_ok());
    }

    #[tokio::test]
    async fn remove_redirect() {
        let mut mock = MockIPTables::new();

        mock.expect_create_chain()
            .with(eq(IPTABLE_PREROUTING.as_str()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(IPTABLE_PREROUTING.as_str()),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_PREROUTING.as_str()))
            .times(1)
            .returning(|_| Ok(()));

        let prerouting = PreroutingRedirect::create(Arc::new(mock)).expect("Unable to create");

        assert!(prerouting.remove_redirect(69, 420).await.is_ok());
    }
}
