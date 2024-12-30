use std::{
    fmt::Debug,
    sync::{Arc, LazyLock},
};

use enum_dispatch::enum_dispatch;
use mirrord_protocol::{MeshVendor, Port};
use rand::distributions::{Alphanumeric, DistString};
use tracing::warn;

use crate::{
    error::{AgentError, Result},
    steal::ip_tables::{
        flush_connections::FlushConnections,
        mesh::{istio::AmbientRedirect, MeshRedirect, MeshVendorExt},
        prerouting::PreroutingRedirect,
        redirect::Redirect,
        standard::StandardRedirect,
    },
};

pub(crate) mod chain;
pub(crate) mod flush_connections;
pub(crate) mod mesh;
pub(crate) mod output;
pub(crate) mod prerouting;
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

pub(crate) static IPTABLE_MESH_ENV: &str = "MIRRORD_IPTABLE_MESH_NAME";
pub(crate) static IPTABLE_MESH: LazyLock<String> = LazyLock::new(|| {
    std::env::var(IPTABLE_MESH_ENV).unwrap_or_else(|_| {
        format!(
            "MIRRORD_OUTPUT_{}",
            Alphanumeric.sample_string(&mut rand::thread_rng(), 5)
        )
    })
});

pub(crate) static IPTABLE_STANDARD_ENV: &str = "MIRRORD_IPTABLE_STANDARD_NAME";
pub(crate) static IPTABLE_STANDARD: LazyLock<String> = LazyLock::new(|| {
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

pub static IPTABLE_IPV4_ROUTE_LOCALNET_ORIGINAL_ENV: &str = "IPTABLE_IPV4_ROUTE_LOCALNET_ORIGINAL";
pub static IPTABLE_IPV4_ROUTE_LOCALNET_ORIGINAL: LazyLock<String> = LazyLock::new(|| {
    std::env::var(IPTABLE_IPV4_ROUTE_LOCALNET_ORIGINAL_ENV).unwrap_or_else(|_| {
        std::fs::read_to_string("/proc/sys/net/ipv4/conf/all/route_localnet")
            .unwrap_or_else(|_| "0".to_string())
    })
});

const IPTABLES_TABLE_NAME: &str = "nat";
const IP6TABLES_TABLE_NAME: &str = "filter";

#[cfg_attr(test, allow(clippy::indexing_slicing))] // `mockall::automock` violates our clippy rules
#[cfg_attr(test, mockall::automock)]
pub(crate) trait IPTables {
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

/// wrapper around iptables::new that uses nft or legacy based on env
pub fn new_iptables() -> iptables::IPTables {
    if let Ok(val) = std::env::var("MIRRORD_AGENT_NFTABLES")
        && val.to_lowercase() == "true"
    {
        iptables::new_with_cmd("/usr/sbin/iptables-nft")
    } else {
        iptables::new_with_cmd("/usr/sbin/iptables-legacy")
    }
    .expect("IPTables initialization may not fail!")
}

/// wrapper around iptables::new that uses nft or legacy based on env
pub fn new_ip6tables_wrapper() -> IPTablesWrapper {
    let ip6tables = if let Ok(val) = std::env::var("MIRRORD_AGENT_NFTABLES")
        && val.to_lowercase() == "true"
    {
        // TODO: check if there is such a binary.
        iptables::new_with_cmd("/usr/sbin/ip6tables-nft")
    } else {
        // TODO: check if there is such a binary.
        iptables::new_with_cmd("/usr/sbin/ip6tables-legacy")
    }
    .expect("IPTables initialization may not fail!");
    IPTablesWrapper {
        table_name: IP6TABLES_TABLE_NAME,
        tables: Arc::new(ip6tables),
    }
}

impl Debug for IPTablesWrapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IPTablesWrapper")
            .field("table_name", &self.table_name)
            .finish()
    }
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
    #[tracing::instrument(level = "trace")]
    fn with_table(&self, table_name: &'static str) -> Self
    where
        Self: Sized,
    {
        IPTablesWrapper {
            table_name,
            tables: self.tables.clone(),
        }
    }

    #[tracing::instrument(level = "debug", skip(self), ret, fields(table_name=%self.table_name))] // TODO: change to trace.
    fn create_chain(&self, name: &str) -> Result<()> {
        self.tables
            .new_chain(self.table_name, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;
        self.tables
            .append(self.table_name, name, "-j RETURN")
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;

        Ok(())
    }

    #[tracing::instrument(level = "trace")]
    fn remove_chain(&self, name: &str) -> Result<()> {
        self.tables
            .flush_chain(self.table_name, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;
        self.tables
            .delete_chain(self.table_name, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;

        Ok(())
    }

    #[tracing::instrument(level = "trace", ret)]
    fn add_rule(&self, chain: &str, rule: &str) -> Result<()> {
        self.tables
            .append(self.table_name, chain, rule)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    #[tracing::instrument(level = "trace", ret)]
    fn insert_rule(&self, chain: &str, rule: &str, index: i32) -> Result<()> {
        self.tables
            .insert(self.table_name, chain, rule, index)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    #[tracing::instrument(level = "trace")]
    fn list_rules(&self, chain: &str) -> Result<Vec<String>> {
        self.tables
            .list(self.table_name, chain)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    #[tracing::instrument(level = "trace")]
    fn remove_rule(&self, chain: &str, rule: &str) -> Result<()> {
        self.tables
            .delete(self.table_name, chain, rule)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }
}

#[enum_dispatch(Redirect)]
pub(crate) enum Redirects<IPT: IPTables + Send + Sync> {
    Ambient(AmbientRedirect<IPT>),
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
    pub(super) async fn create(
        ipt: IPT,
        flush_connections: bool,
        pod_ips: Option<&str>,
        ipv6: bool,
    ) -> Result<Self> {
        let ipt = Arc::new(ipt);

        let mut redirect = if let Some(vendor) = MeshVendor::detect(ipt.as_ref())? {
            match &vendor {
                MeshVendor::IstioAmbient => {
                    Redirects::Ambient(AmbientRedirect::create(ipt.clone(), pod_ips)?)
                }
                _ => Redirects::Mesh(MeshRedirect::create(ipt.clone(), vendor, pod_ips)?),
            }
        } else {
            tracing::debug!(ipv6 = ipv6, "creating standard redirect"); // TODO: change to trace.
            match StandardRedirect::create(ipt.clone(), pod_ips, ipv6) {
                Err(err) => {
                    warn!("Unable to create StandardRedirect chain: {err}");

                    Redirects::PrerouteFallback(PreroutingRedirect::create_prerouting(ipt.clone())?)
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
            match &vendor {
                MeshVendor::IstioAmbient => Redirects::Ambient(AmbientRedirect::load(ipt.clone())?),
                _ => Redirects::Mesh(MeshRedirect::load(ipt.clone(), vendor)?),
            }
        } else {
            match StandardRedirect::load(ipt.clone()) {
                Err(err) => {
                    warn!("Unable to load StandardRedirect chain: {err}");

                    Redirects::PrerouteFallback(PreroutingRedirect::load_prerouting(ipt.clone())?)
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
    #[tracing::instrument(level = tracing::Level::DEBUG, skip(self))]
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

        let ipt = SafeIpTables::create(mock, false, None)
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
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
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
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
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

        let ipt = SafeIpTables::create(mock, false, None)
            .await
            .expect("Create Failed");

        assert!(ipt.add_redirect(69, 420).await.is_ok());

        assert!(ipt.remove_redirect(69, 420).await.is_ok());

        assert!(ipt.cleanup().await.is_ok());
    }
}
