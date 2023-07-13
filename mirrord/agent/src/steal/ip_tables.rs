use std::sync::{Arc, LazyLock};

use enum_dispatch::enum_dispatch;
use mirrord_protocol::Port;
use rand::distributions::{Alphanumeric, DistString};
use tracing::warn;

use crate::{
    error::{AgentError, Result},
    steal::ip_tables::{
        flush_connections::FlushConnections,
        mesh::{MeshRedirect, MeshVendor},
        redirect::{PreroutingRedirect, Redirect},
        standard::StandardRedirect,
    },
};

#[cfg(not(target_os = "linux"))]
mod iptables {
    pub struct IPTables;

    impl IPTables {
        pub fn list(&self, _: &str, _: &str) -> Result<Vec<String>, String> {
            todo!()
        }
        pub fn insert(&self, _: &str, _: &str, _: &str, _: i32) -> Result<(), String> {
            todo!()
        }
        pub fn append(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
            todo!()
        }
        pub fn delete(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
            todo!()
        }

        pub fn new_chain(&self, _: &str, _: &str) -> Result<(), String> {
            todo!()
        }
        pub fn delete_chain(&self, _: &str, _: &str) -> Result<(), String> {
            todo!()
        }
        pub fn flush_chain(&self, _: &str, _: &str) -> Result<(), String> {
            todo!()
        }
    }
}

pub(crate) mod chain;
pub(crate) mod flush_connections;
pub(crate) mod mesh;
pub(crate) mod redirect;
pub(crate) mod standard;

pub static IPTABLE_PREROUTING_ENV: &str = "MIRRORD_IPTABLE_PREROUTING_NAME";
pub static IPTABLE_PREROUTING: LazyLock<String> = LazyLock::new(|| {
    std::env::var(IPTABLE_PREROUTING_ENV).unwrap_or_else(|_| {
        format!(
            "MIRRORD_INPUT_{}",
            Alphanumeric.sample_string(&mut rand::thread_rng(), 5)
        )
    })
});

pub static IPTABLE_MESH_ENV: &str = "MIRRORD_IPTABLE_MESH_NAME";
pub static IPTABLE_MESH: LazyLock<String> = LazyLock::new(|| {
    std::env::var(IPTABLE_MESH_ENV).unwrap_or_else(|_| {
        format!(
            "MIRRORD_OUTPUT_{}",
            Alphanumeric.sample_string(&mut rand::thread_rng(), 5)
        )
    })
});

pub static IPTABLE_STANDARD_ENV: &str = "MIRRORD_IPTABLE_STANDARD_NAME";
pub static IPTABLE_STANDARD: LazyLock<String> = LazyLock::new(|| {
    std::env::var(IPTABLE_STANDARD_ENV).unwrap_or_else(|_| {
        format!(
            "MIRRORD_STANDARD_{}",
            Alphanumeric.sample_string(&mut rand::thread_rng(), 5)
        )
    })
});

pub static IPTABLE_INPUT_ENV: &str = "MIRRORD_IPTABLE_INPUT_NAME";
pub static IPTABLE_INPUT: LazyLock<String> = LazyLock::new(|| {
    std::env::var(IPTABLE_INPUT_ENV).unwrap_or_else(|_| {
        format!(
            "MIRRORD_INPUT_{}",
            Alphanumeric.sample_string(&mut rand::thread_rng(), 5)
        )
    })
});

const IPTABLES_TABLE_NAME: &str = "nat";

#[cfg_attr(test, mockall::automock)]
pub trait IPTables {
    fn with_table(&self, table_name: &'static str) -> Self
    where
        Self: Sized;

    fn create_chain(&self, name: &str) -> Result<()>;
    fn remove_chain(&self, name: &str) -> Result<()>;

    fn add_rule(&self, chain: &str, rule: &str) -> Result<()>;
    fn insert_rule(&self, chain: &str, rule: &str, index: i32) -> Result<()>;
    fn list_rules(&self, chain: &str) -> Result<Vec<String>>;
    fn remove_rule(&self, chain: &str, rule: &str) -> Result<()>;
}

#[derive(Clone)]
pub struct IPTablesWrapper {
    table_name: &'static str,
    tables: Arc<iptables::IPTables>,
}

impl From<iptables::IPTables> for IPTablesWrapper {
    fn from(tables: iptables::IPTables) -> Self {
        IPTablesWrapper {
            table_name: IPTABLES_TABLE_NAME,
            tables: Arc::new(tables),
        }
    }
}

impl IPTables for IPTablesWrapper {
    #[tracing::instrument(level = "trace", skip(self))]
    fn with_table(&self, table_name: &'static str) -> Self
    where
        Self: Sized,
    {
        IPTablesWrapper {
            table_name,
            tables: self.tables.clone(),
        }
    }

    #[tracing::instrument(level = "trace", skip(self))]
    fn create_chain(&self, name: &str) -> Result<()> {
        self.tables
            .new_chain(self.table_name, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;
        self.tables
            .append(self.table_name, name, "-j RETURN")
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;

        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self))]
    fn remove_chain(&self, name: &str) -> Result<()> {
        self.tables
            .flush_chain(self.table_name, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;
        self.tables
            .delete_chain(self.table_name, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;

        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self))]
    fn add_rule(&self, chain: &str, rule: &str) -> Result<()> {
        self.tables
            .append(self.table_name, chain, rule)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    #[tracing::instrument(level = "trace", skip(self))]
    fn insert_rule(&self, chain: &str, rule: &str, index: i32) -> Result<()> {
        self.tables
            .insert(self.table_name, chain, rule, index)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    #[tracing::instrument(level = "trace", skip(self))]
    fn list_rules(&self, chain: &str) -> Result<Vec<String>> {
        self.tables
            .list(self.table_name, chain)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    #[tracing::instrument(level = "trace", skip(self))]
    fn remove_rule(&self, chain: &str, rule: &str) -> Result<()> {
        self.tables
            .delete(self.table_name, chain, rule)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }
}

#[enum_dispatch(Redirect)]
pub enum Redirects<IPT: IPTables + Send + Sync> {
    Standard(StandardRedirect<IPT>),
    Mesh(MeshRedirect<IPT>),
    FlushConnections(FlushConnections<IPT, Redirects<IPT>>),
    PrerouteFallback(PreroutingRedirect<IPT>),
}

/// Wrapper struct for IPTables so it flushes on drop.
pub(crate) struct SafeIpTables<IPT: IPTables + Send + Sync> {
    redirect: Redirects<IPT>,
}

/// Wrapper for using iptables. This creates a a new chain on creation and deletes it on drop.
/// The way it works is that it adds a chain, then adds a rule to the chain that returns to the
/// original chain (fallback) and adds a rule in the "PREROUTING" table that jumps to the new chain.
/// Connections will go then PREROUTING -> OUR_CHAIN -> IF MATCH REDIRECT -> IF NOT MATCH FALLBACK
/// -> ORIGINAL_CHAIN
impl<IPT> SafeIpTables<IPT>
where
    IPT: IPTables + Send + Sync,
{
    pub(super) async fn create(ipt: IPT, flush_connections: bool) -> Result<Self> {
        let ipt = Arc::new(ipt);

        let mut redirect = if let Some(vendor) = MeshVendor::detect(ipt.as_ref())? {
            Redirects::Mesh(MeshRedirect::create(ipt.clone(), vendor)?)
        } else {
            match StandardRedirect::create(ipt.clone()) {
                Err(err) => {
                    warn!("Unable to create StandardRedirect chain: {err}");

                    Redirects::PrerouteFallback(PreroutingRedirect::create(ipt.clone())?)
                }
                Ok(standard) => Redirects::Standard(standard),
            }
        };

        if flush_connections {
            redirect =
                Redirects::FlushConnections(FlushConnections::create(ipt, Box::new(redirect))?)
        }

        redirect.mount_entrypoint().await?;

        Ok(Self { redirect })
    }

    pub(crate) async fn load(ipt: IPT, flush_connections: bool) -> Result<Self> {
        let ipt = Arc::new(ipt);

        let mut redirect = if let Some(vendor) = MeshVendor::detect(ipt.as_ref())? {
            Redirects::Mesh(MeshRedirect::load(ipt.clone(), vendor)?)
        } else {
            match StandardRedirect::load(ipt.clone()) {
                Err(err) => {
                    warn!("Unable to load StandardRedirect chain: {err}");

                    Redirects::PrerouteFallback(PreroutingRedirect::load(ipt.clone())?)
                }
                Ok(standard) => Redirects::Standard(standard),
            }
        };

        if flush_connections {
            redirect = Redirects::FlushConnections(FlushConnections::load(ipt, Box::new(redirect))?)
        }

        Ok(Self { redirect })
    }

    /// Adds the redirect rule to iptables.
    ///
    /// Used to redirect packets when mirrord incoming feature is set to `steal`.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) async fn add_redirect(
        &self,
        redirected_port: Port,
        target_port: Port,
    ) -> Result<()> {
        self.redirect
            .add_redirect(redirected_port, target_port)
            .await
    }

    /// Removes the redirect rule from iptables.
    ///
    /// Stops redirecting packets when mirrord incoming feature is set to `steal`, and there are no
    /// more subscribers on `target_port`.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) async fn remove_redirect(
        &self,
        redirected_port: Port,
        target_port: Port,
    ) -> Result<()> {
        self.redirect
            .remove_redirect(redirected_port, target_port)
            .await
    }

    pub(crate) async fn cleanup(&self) -> Result<()> {
        self.redirect.unmount_entrypoint().await
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::*;

    use super::*;

    #[tokio::test]
    async fn default() {
        let mut mock = MockIPTables::new();

        mock.expect_list_rules()
            .with(eq("OUTPUT"))
            .returning(|_| Ok(vec![]));

        mock.expect_create_chain()
            .with(str::starts_with("MIRRORD_INPUT_"))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                str::starts_with("MIRRORD_INPUT_"),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_create_chain()
            .with(str::starts_with("MIRRORD_STANDARD_"))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                str::starts_with("MIRRORD_STANDARD_"),
                str::starts_with("-m owner --gid-owner"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                str::starts_with("MIRRORD_STANDARD_"),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_add_rule()
            .with(eq("PREROUTING"), str::starts_with("-j MIRRORD_INPUT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_add_rule()
            .with(eq("OUTPUT"), str::starts_with("-j MIRRORD_STANDARD_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                str::starts_with("MIRRORD_INPUT_"),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                str::starts_with("MIRRORD_STANDARD_"),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(eq("PREROUTING"), str::starts_with("-j MIRRORD_INPUT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(eq("OUTPUT"), str::starts_with("-j MIRRORD_STANDARD_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(str::starts_with("MIRRORD_INPUT_"))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_chain()
            .with(str::starts_with("MIRRORD_STANDARD_"))
            .times(1)
            .returning(|_| Ok(()));

        let ipt = SafeIpTables::create(mock, false)
            .await
            .expect("Create Failed");

        assert!(ipt.add_redirect(69, 420).await.is_ok());

        assert!(ipt.remove_redirect(69, 420).await.is_ok());

        assert!(ipt.cleanup().await.is_ok());
    }

    #[tokio::test]
    async fn linkerd() {
        let mut mock = MockIPTables::new();

        mock.expect_list_rules()
            .with(eq("OUTPUT"))
            .returning(|_| Ok(vec!["-j PROXY_INIT_OUTPUT".to_owned()]));

        mock.expect_list_rules()
            .with(eq("PROXY_INIT_REDIRECT"))
            .returning(|_| {
                Ok(vec![
                    "-N PROXY_INIT_REDIRECT".to_owned(),
                    "-A PROXY_INIT_REDIRECT -p tcp -m multiport --dports 22 -j RETURN".to_owned(),
                    "-A PROXY_INIT_REDIRECT -p tcp -j REDIRECT --to-port 4143".to_owned(),
                ])
            });

        mock.expect_list_rules()
            .with(eq("PROXY_INIT_OUTPUT"))
            .returning(|_| {
                Ok(vec![
                    "-N PROXY_INIT_OUTPUT".to_owned(),
                    "-A PROXY_INIT_OUTPUT -m owner --uid-owner 2102 -m comment --comment \"proxy-init/ignore-proxy-user-id/1676542558\" -j RETURN"
                        .to_owned(),
                    "-A PROXY_INIT_OUTPUT -o lo -m comment --comment \"proxy-init/ignore-loopback/1676542558\" -js RETURN"
                        .to_owned(),
                ])
            });

        mock.expect_create_chain()
            .with(str::starts_with("MIRRORD_INPUT_"))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                str::starts_with("MIRRORD_INPUT_"),
                eq("-m multiport -p tcp ! --dports 22 -j RETURN"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_add_rule()
            .with(eq("PREROUTING"), str::starts_with("-j MIRRORD_INPUT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_create_chain()
            .with(str::starts_with("MIRRORD_OUTPUT_"))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                str::starts_with("MIRRORD_OUTPUT_"),
                str::starts_with("-m owner --gid-owner"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_add_rule()
            .with(eq("OUTPUT"), str::starts_with("-j MIRRORD_OUTPUT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                str::starts_with("MIRRORD_INPUT_"),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                str::starts_with("MIRRORD_OUTPUT_"),
                eq(
                    "-o lo -m owner --uid-owner 2102 -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420",
                ),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                str::starts_with("MIRRORD_INPUT_"),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                str::starts_with("MIRRORD_OUTPUT_"),
                eq(
                    "-o lo -m owner --uid-owner 2102 -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420",
                ),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(eq("PREROUTING"), str::starts_with("-j MIRRORD_INPUT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(str::starts_with("MIRRORD_INPUT_"))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_rule()
            .with(eq("OUTPUT"), str::starts_with("-j MIRRORD_OUTPUT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(str::starts_with("MIRRORD_OUTPUT_"))
            .times(1)
            .returning(|_| Ok(()));

        let ipt = SafeIpTables::create(mock, false)
            .await
            .expect("Create Failed");

        assert!(ipt.add_redirect(69, 420).await.is_ok());

        assert!(ipt.remove_redirect(69, 420).await.is_ok());

        assert!(ipt.cleanup().await.is_ok());
    }
}
