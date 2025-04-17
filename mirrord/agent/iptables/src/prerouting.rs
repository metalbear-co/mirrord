use std::{ops::Deref, sync::Arc};

use async_trait::async_trait;

use crate::{chain::IPTableChain, error::IPTablesResult, IPTables, Redirect, IPTABLE_PREROUTING};

pub struct PreroutingRedirect<IPT: IPTables> {
    managed: IPTableChain<IPT>,
}

impl<IPT> PreroutingRedirect<IPT>
where
    IPT: IPTables,
{
    const ENTRYPOINT: &'static str = "PREROUTING";

    pub fn create(ipt: Arc<IPT>) -> IPTablesResult<Self> {
        let managed = IPTableChain::create(ipt, IPTABLE_PREROUTING.to_string())?;

        Ok(PreroutingRedirect { managed })
    }

    pub fn load(ipt: Arc<IPT>) -> IPTablesResult<Self> {
        let managed = IPTableChain::load(ipt, IPTABLE_PREROUTING.to_string())?;

        Ok(PreroutingRedirect { managed })
    }
}

#[async_trait]
impl<IPT> Redirect for PreroutingRedirect<IPT>
where
    IPT: IPTables + Send + Sync,
{
    async fn mount_entrypoint(&self) -> IPTablesResult<()> {
        self.managed.inner().add_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.managed.chain_name()),
        )?;

        Ok(())
    }

    async fn unmount_entrypoint(&self) -> IPTablesResult<()> {
        self.managed.inner().remove_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.managed.chain_name()),
        )?;

        Ok(())
    }

    async fn add_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()> {
        let redirect_rule =
            format!("-m tcp -p tcp --dport {redirected_port} -j REDIRECT --to-ports {target_port}");

        self.managed.add_rule(&redirect_rule)?;

        Ok(())
    }

    async fn remove_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()> {
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
    use std::sync::Arc;

    use mockall::predicate::eq;

    use crate::{
        prerouting::PreroutingRedirect, redirect::Redirect, MockIPTables, IPTABLE_PREROUTING,
    };

    #[tokio::test]
    async fn add_redirect() {
        let mut mock = MockIPTables::new();

        mock.expect_create_chain()
            .with(eq(IPTABLE_PREROUTING))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_PREROUTING),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_PREROUTING))
            .times(1)
            .returning(|_| Ok(()));

        let prerouting = PreroutingRedirect::create(Arc::new(mock)).expect("Unable to create");

        assert!(prerouting.add_redirect(69, 420).await.is_ok());
    }

    #[tokio::test]
    async fn add_redirect_twice() {
        let mut mock = MockIPTables::new();

        mock.expect_create_chain()
            .with(eq(IPTABLE_PREROUTING))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_PREROUTING),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_PREROUTING),
                eq("-m tcp -p tcp --dport 169 -j REDIRECT --to-ports 1420"),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_PREROUTING))
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
            .with(eq(IPTABLE_PREROUTING))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(IPTABLE_PREROUTING),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_PREROUTING))
            .times(1)
            .returning(|_| Ok(()));

        let prerouting = PreroutingRedirect::create(Arc::new(mock)).expect("Unable to create");

        assert!(prerouting.remove_redirect(69, 420).await.is_ok());
    }
}
