use std::{
    fmt::Debug,
    sync::{Arc, LazyLock, OnceLock},
};

use enum_dispatch::enum_dispatch;
use mirrord_agent_env::mesh::MeshVendor;
use tracing::{Level, warn};

use crate::{
    error::IPTablesResult,
    flush_connections::FlushConnections,
    mesh::{
        MeshRedirect, MeshVendorExt,
        exclusion::{MeshExclusion, WithMeshExclusion},
        istio::AmbientRedirect,
    },
    prerouting::PreroutingRedirect,
    redirect::Redirect,
    standard::StandardRedirect,
};

mod chain;
pub mod error;
mod flush_connections;
mod mesh;
mod output;
mod prerouting;
mod redirect;
mod standard;

pub const IPTABLE_PREROUTING: &str = "MIRRORD_INPUT";

pub const IPTABLE_MESH: &str = "MIRRORD_OUTPUT";

pub const IPTABLE_STANDARD: &str = "MIRRORD_STANDARD";

pub const IPTABLE_EXCLUDE_FROM_MESH: &str = "MIRRORD_EXCLUDE_FROM_MESH";

pub static IPTABLE_IPV4_ROUTE_LOCALNET_ORIGINAL: LazyLock<String> = LazyLock::new(|| {
    std::fs::read_to_string("/proc/sys/net/ipv4/conf/all/route_localnet")
        .unwrap_or_else(|_| "0".to_string())
});

const IPTABLES_TABLE_NAME: &str = "nat";

#[cfg_attr(test, allow(clippy::indexing_slicing))] // `mockall::automock` violates our clippy rules
#[cfg_attr(test, mockall::automock)]
pub trait IPTables {
    fn with_table(&self, table_name: &'static str) -> Self
    where
        Self: Sized;

    fn create_chain(&self, name: &str) -> IPTablesResult<()>;
    fn remove_chain(&self, name: &str) -> IPTablesResult<()>;

    fn add_rule(&self, chain: &str, rule: &str) -> IPTablesResult<()>;
    fn insert_rule(&self, chain: &str, rule: &str, index: i32) -> IPTablesResult<()>;
    fn list_rules(&self, chain: &str) -> IPTablesResult<Vec<String>>;

    fn list_table(&self) -> IPTablesResult<Vec<String>>;
    fn remove_rule(&self, chain: &str, rule: &str) -> IPTablesResult<()>;
}

#[derive(Clone)]
pub struct IPTablesWrapper {
    table_name: &'static str,
    tables: Arc<iptables::IPTables>,
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
    fn with_table(&self, table_name: &'static str) -> Self
    where
        Self: Sized,
    {
        IPTablesWrapper {
            table_name,
            tables: self.tables.clone(),
        }
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    fn create_chain(&self, name: &str) -> IPTablesResult<()> {
        self.tables.new_chain(self.table_name, name)?;
        self.tables.append(self.table_name, name, "-j RETURN")?;

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    fn remove_chain(&self, name: &str) -> IPTablesResult<()> {
        self.tables.flush_chain(self.table_name, name)?;
        self.tables.delete_chain(self.table_name, name)?;

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    fn add_rule(&self, chain: &str, rule: &str) -> IPTablesResult<()> {
        self.tables
            .append(self.table_name, chain, rule)
            .map_err(From::from)
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    fn insert_rule(&self, chain: &str, rule: &str, index: i32) -> IPTablesResult<()> {
        self.tables
            .insert(self.table_name, chain, rule, index)
            .map_err(From::from)
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    fn list_rules(&self, chain: &str) -> IPTablesResult<Vec<String>> {
        self.tables.list(self.table_name, chain).map_err(From::from)
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    fn list_table(&self) -> IPTablesResult<Vec<String>> {
        self.tables.list_table(self.table_name).map_err(From::from)
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    fn remove_rule(&self, chain: &str, rule: &str) -> IPTablesResult<()> {
        self.tables
            .delete(self.table_name, chain, rule)
            .map_err(From::from)
    }
}

#[enum_dispatch(Redirect)]
enum Redirects<IPT: IPTables + Send + Sync> {
    Ambient(AmbientRedirect<IPT>),
    Standard(StandardRedirect<IPT>),
    Mesh(MeshRedirect<IPT>),
    FlushConnections(FlushConnections<Redirects<IPT>>),
    PrerouteFallback(PreroutingRedirect<IPT>),
    WithMeshExclusion(WithMeshExclusion<IPT, Redirects<IPT>>),
}

/// Wrapper struct for IPTables so it flushes on drop.
pub struct SafeIpTables<IPT: IPTables + Send + Sync> {
    redirect: Redirects<IPT>,
}

/// Wrapper for using iptables. This creates a new chain on creation and deletes it on drop.
/// The way it works is that it adds a chain, then adds a rule to the chain that returns to the
/// original chain (fallback) and adds a rule in the "PREROUTING" table that jumps to the new chain.
/// Connections will then go PREROUTING -> OUR_CHAIN -> IF MATCH REDIRECT -> IF NOT MATCH FALLBACK
/// -> ORIGINAL_CHAIN
impl<IPT> SafeIpTables<IPT>
where
    IPT: IPTables + Send + Sync,
{
    pub async fn create(
        ipt: IPT,
        flush_connections: bool,
        pod_ips: Option<&str>,
        ipv6: bool,
        with_mesh_exclusion: bool,
    ) -> IPTablesResult<Self> {
        let ipt = Arc::new(ipt);

        let mut redirect = match MeshVendor::detect(ipt.as_ref())? {
            Some(vendor) => match &vendor {
                MeshVendor::IstioAmbient => {
                    Redirects::Ambient(AmbientRedirect::create(ipt.clone(), pod_ips)?)
                }
                _ => Redirects::Mesh(MeshRedirect::create(ipt.clone(), vendor, pod_ips)?),
            },
            _ => {
                tracing::trace!(ipv6 = ipv6, "creating standard redirect");
                match StandardRedirect::create(ipt.clone(), pod_ips) {
                    Err(err) => {
                        warn!("Unable to create StandardRedirect chain: {err}");

                        Redirects::PrerouteFallback(PreroutingRedirect::create(ipt.clone())?)
                    }
                    Ok(standard) => Redirects::Standard(standard),
                }
            }
        };

        if flush_connections {
            redirect = Redirects::FlushConnections(FlushConnections::create(Box::new(redirect))?)
        }

        // Should be always the last composed redirect because it handles the order internally.
        if with_mesh_exclusion {
            redirect =
                Redirects::WithMeshExclusion(WithMeshExclusion::create(ipt, Box::new(redirect))?)
        }

        redirect.mount_entrypoint().await?;

        Ok(Self { redirect })
    }

    /// List rules from other/ previous mirrord agents that exist on the IP table
    #[tracing::instrument(level = Level::TRACE, skip(ipt) ret, err)]
    pub async fn list_mirrord_rules(ipt: &IPT) -> IPTablesResult<Vec<String>> {
        let rules = ipt.list_table()?;

        Ok(rules
            .into_iter()
            .filter(|rule| {
                [
                    IPTABLE_PREROUTING,
                    IPTABLE_MESH,
                    IPTABLE_STANDARD,
                    IPTABLE_EXCLUDE_FROM_MESH,
                ]
                .iter()
                .any(|chain| rule.contains(*chain))
            })
            .collect())
    }

    pub async fn load(
        ipt: IPT,
        flush_connections: bool,
        with_mesh_exclusion: bool,
    ) -> IPTablesResult<Self> {
        let ipt = Arc::new(ipt);

        let mut redirect = match MeshVendor::detect(ipt.as_ref())? {
            Some(vendor) => match &vendor {
                MeshVendor::IstioAmbient => Redirects::Ambient(AmbientRedirect::load(ipt.clone())?),
                _ => Redirects::Mesh(MeshRedirect::load(ipt.clone(), vendor)?),
            },
            _ => match StandardRedirect::load(ipt.clone()) {
                Err(err) => {
                    warn!("Unable to load StandardRedirect chain: {err}");

                    Redirects::PrerouteFallback(PreroutingRedirect::load(ipt.clone())?)
                }
                Ok(standard) => Redirects::Standard(standard),
            },
        };

        if flush_connections {
            redirect = Redirects::FlushConnections(FlushConnections::load(Box::new(redirect))?)
        }

        // Should be always the last composed redirect because it handles the order internally.
        if with_mesh_exclusion {
            redirect =
                Redirects::WithMeshExclusion(WithMeshExclusion::load(ipt, Box::new(redirect))?)
        }

        Ok(Self { redirect })
    }

    /// Adds the redirect rule to iptables.
    ///
    /// Used to redirect packets when mirrord incoming feature is set to `steal`.
    #[tracing::instrument(level = Level::DEBUG, skip(self), err)]
    pub async fn add_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()> {
        self.redirect
            .add_redirect(redirected_port, target_port)
            .await
    }

    /// Removes the redirect rule from iptables.
    ///
    /// Stops redirecting packets when mirrord incoming feature is set to `steal`, and there are no
    /// more subscribers on `target_port`.
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub async fn remove_redirect(
        &self,
        redirected_port: u16,
        target_port: u16,
    ) -> IPTablesResult<()> {
        self.redirect
            .remove_redirect(redirected_port, target_port)
            .await
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub async fn cleanup(&self) -> IPTablesResult<()> {
        self.redirect.unmount_entrypoint().await
    }

    pub fn exclusion(&self) -> Option<&MeshExclusion<IPT>> {
        match &self.redirect {
            Redirects::WithMeshExclusion(redirect) => Some(redirect.exclusion()),
            _ => None,
        }
    }
}

/// Returns correct [`IPTablesWrapper`] to use for traffic redirection.
///
/// If `nftables` is `false`, this function will return the ip[6]tables-legacy wrapper.
///
/// If `nftables` is `true`, this function will return the ip[6]tables-nft wrapper.
///
/// If `nftables` is [`None`], this function will choose between legacy and nftables:
/// 1. If mesh rules are found with ip[6]tables-nft, ip[6]tables-nft wrapper will be returned.
/// 2. Otherwise, ip[6]tables-legacy wrapper will be returned.
pub fn get_iptables(nftables: Option<bool>, ip6: bool) -> IPTablesWrapper {
    /// Whether we should use nftables when no backend is explicitly configured.
    ///
    /// Initialized with the first call of this function, when the `nftables` argument is not
    /// provided.
    static DETECTED_NFTABLES: OnceLock<bool> = OnceLock::new();

    // If `nftables` or `DETECTED_NFTABLES` is set, always return early.
    // This function calls itself recursively later.
    let nftables = nftables.or_else(|| DETECTED_NFTABLES.get().copied());
    if let Some(nftables) = nftables {
        let path = match (nftables, ip6) {
            (true, true) => "/usr/sbin/ip6tables-nft",
            (true, false) => "/usr/sbin/iptables-nft",
            (false, true) => "/usr/sbin/ip6tables-legacy",
            (false, false) => "/usr/sbin/iptables-legacy",
        };
        return iptables::new_with_cmd(path)
            .expect("IPTables initialization should not fail, the binary should be present in the agent image")
            .into();
    }

    let nft_wrapper = get_iptables(Some(true), ip6);
    let nft_rules_present = MeshVendor::detect(&nft_wrapper)
        .inspect_err(|error| {
            tracing::warn!(
                %error,
                command = nft_wrapper.tables.cmd,
                "Failed to detect mesh rules with nftables, \
                assuming no mesh rules and falling back to legacy iptables.",
            );
        })
        .is_ok_and(|detected_mesh| detected_mesh.is_some());
    let _ = DETECTED_NFTABLES.set(nft_rules_present);

    if nft_rules_present {
        nft_wrapper
    } else {
        get_iptables(Some(false), ip6)
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::{eq, str};

    use crate::{
        IPTABLE_EXCLUDE_FROM_MESH, IPTABLE_MESH, IPTABLE_PREROUTING, IPTABLE_STANDARD,
        MockIPTables, SafeIpTables,
    };

    #[tokio::test]
    async fn default() {
        let mut mock = MockIPTables::new();

        mock.expect_list_rules()
            .with(eq("OUTPUT"))
            .returning(|_| Ok(vec![]));

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

        mock.expect_create_chain()
            .with(eq(IPTABLE_STANDARD))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_STANDARD),
                str::starts_with("-m owner --gid-owner"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_STANDARD),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_add_rule()
            .with(eq("PREROUTING"), eq(format!("-j {}", IPTABLE_PREROUTING)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_add_rule()
            .with(eq("OUTPUT"), eq(format!("-j {}", IPTABLE_STANDARD)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(IPTABLE_PREROUTING),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(IPTABLE_STANDARD),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(eq("PREROUTING"), eq(format!("-j {}", IPTABLE_PREROUTING)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(eq("OUTPUT"), eq(format!("-j {}", IPTABLE_STANDARD)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_PREROUTING))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_STANDARD))
            .times(1)
            .returning(|_| Ok(()));

        let ipt = SafeIpTables::create(mock, false, None, false, false)
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
            .with(eq(IPTABLE_PREROUTING))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_PREROUTING),
                eq("-m multiport -p tcp ! --dports 22 -j RETURN"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_add_rule()
            .with(eq("PREROUTING"), eq(format!("-j {}", IPTABLE_PREROUTING)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_create_chain()
            .with(eq(IPTABLE_MESH))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_MESH),
                str::starts_with("-m owner --gid-owner"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_add_rule()
            .with(eq("OUTPUT"), eq(format!("-j {}", IPTABLE_MESH)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_PREROUTING),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_MESH),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(IPTABLE_PREROUTING),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(IPTABLE_MESH),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(eq("PREROUTING"), eq(format!("-j {}", IPTABLE_PREROUTING)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_PREROUTING))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_rule()
            .with(eq("OUTPUT"), eq(format!("-j {}", IPTABLE_MESH)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_MESH))
            .times(1)
            .returning(|_| Ok(()));

        let ipt = SafeIpTables::create(mock, false, None, false, false)
            .await
            .expect("Create Failed");

        assert!(ipt.add_redirect(69, 420).await.is_ok());

        assert!(ipt.remove_redirect(69, 420).await.is_ok());

        assert!(ipt.cleanup().await.is_ok());
    }

    #[tokio::test]
    async fn with_mesh_exclusion() {
        let mut mock = MockIPTables::new();

        mock.expect_list_rules()
            .with(eq("OUTPUT"))
            .returning(|_| Ok(vec![]));

        mock.expect_create_chain()
            .with(eq(IPTABLE_EXCLUDE_FROM_MESH))
            .times(1)
            .returning(|_| Ok(()));

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

        mock.expect_create_chain()
            .with(eq(IPTABLE_STANDARD))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_STANDARD),
                str::starts_with("-m owner --gid-owner"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_STANDARD),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq("PREROUTING"),
                eq(format!("-j {}", IPTABLE_EXCLUDE_FROM_MESH)),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_add_rule()
            .with(eq("PREROUTING"), eq(format!("-j {}", IPTABLE_PREROUTING)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_add_rule()
            .with(eq("OUTPUT"), eq(format!("-j {}", IPTABLE_STANDARD)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(IPTABLE_PREROUTING),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(IPTABLE_STANDARD),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
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

        mock.expect_remove_rule()
            .with(eq("PREROUTING"), eq(format!("-j {}", IPTABLE_PREROUTING)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(eq("OUTPUT"), eq(format!("-j {}", IPTABLE_STANDARD)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_EXCLUDE_FROM_MESH))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_PREROUTING))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_STANDARD))
            .times(1)
            .returning(|_| Ok(()));

        let ipt = SafeIpTables::create(mock, false, None, false, true)
            .await
            .expect("Create Failed");

        assert!(ipt.add_redirect(69, 420).await.is_ok());

        assert!(ipt.remove_redirect(69, 420).await.is_ok());

        assert!(ipt.cleanup().await.is_ok());
    }

    /// Ensure that clean ip tables pass the ['SafeIpTables::ensure_iptables_clean'] check.
    /// A fresh IP table, or one with only non-agent names, should pass.
    #[tokio::test]
    async fn pass_on_clean() {
        let mut mock = MockIPTables::new();

        // clean table returns non-mirrord rules only
        mock.expect_list_table().with().times(1).returning(|| {
            Ok(vec![
                "-P PREROUTING ACCEPT".to_owned(),
                "-P INPUT ACCEPT".to_owned(),
                "-P OUTPUT ACCEPT".to_owned(),
                "-P POSTROUTING ACCEPT".to_owned(),
            ])
        });

        let leftover_rules_res = SafeIpTables::list_mirrord_rules(&mock).await;
        assert_eq!(
            leftover_rules_res.unwrap().len(),
            0,
            "Fresh IP table should successfully list table rules and list no existing mirrord rules"
        );
    }

    /// Ensure that dirty ip tables fail the ['SafeIpTables::ensure_iptables_clean'] check.
    /// If there are any chains in the IP table with names used by the agent, the check should fail.
    #[tokio::test]
    async fn fail_on_dirty() {
        let mut mock = MockIPTables::new();

        // dirty table returns non-mirrord rules, plus a leftover mirrord rule
        mock.expect_list_table().with().times(1).returning(|| {
            Ok(vec![
                "-P PREROUTING ACCEPT".to_owned(),
                "-P INPUT ACCEPT".to_owned(),
                "-P OUTPUT ACCEPT".to_owned(),
                "-P POSTROUTING ACCEPT".to_owned(),
                format!("-N {IPTABLE_PREROUTING}"),
            ])
        });

        let leftover_rules_res = SafeIpTables::list_mirrord_rules(&mock).await;
        assert_eq!(
            leftover_rules_res.unwrap().len(),
            1,
            "Fresh IP table should successfully list table rules and list one existing mirrord rule"
        );
    }
}
