#![deny(unused_crate_dependencies)]
#![cfg(target_os = "linux")]
use std::{fmt::Debug, sync::Arc};

#[cfg(test)]
use caps as _;
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

#[cfg(not(test))]
mod backend;
mod chain;
pub mod error;
mod flush_connections;
mod mesh;
mod output;
mod prerouting;
mod redirect;
mod standard;

#[cfg(not(test))]
pub use backend::{get_iptables, warn_on_backend_mesh_mismatch};

#[cfg(not(test))]
use self::IPTablesWrapper as IPTablesBackend;
#[cfg(test)]
use self::MockIPTablesWrapper as IPTablesBackend;

/// Holds the iptables chain names for this agent instance.
///
/// When `MIRRORD_AGENT_IPTABLES_IDENTIFIER` is set, allows multiple agents to coexist in the same
/// network namespace (single pod, multiple containers).
///
/// When the env var is absent, legacy names are used for backward compatibility.
#[derive(Debug, Clone)]
pub struct ChainNames {
    prerouting: String,
    mesh: String,
    standard: String,
    exclude_from_mesh: String,
}

impl ChainNames {
    #[tracing::instrument(level = Level::DEBUG, ret)]
    pub fn new(id: &str) -> Self {
        Self {
            prerouting: format!("MRDIN_{id}"),
            mesh: format!("MRDOUT_{id}"),
            standard: format!("MRDSTD_{id}"),
            exclude_from_mesh: format!("MRDMSH_{id}"),
        }
    }

    /// Our legacy static iptables' rules.
    ///
    /// Mostly used to clean-up old rules from agents that do not support multi-container targeting
    /// (and for tests).
    pub fn legacy() -> Self {
        Self {
            prerouting: "MIRRORD_INPUT".to_owned(),
            mesh: "MIRRORD_OUTPUT".to_owned(),
            standard: "MIRRORD_STANDARD".to_owned(),
            exclude_from_mesh: "MIRRORD_EXCLUDE_FROM_MESH".to_owned(),
        }
    }
}

const IPTABLES_TABLE_NAME: &str = "nat";

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

#[cfg_attr(test, allow(clippy::indexing_slicing))] // `mockall::automock` violates our clippy rules
#[cfg_attr(test, mockall::automock)]
impl IPTablesWrapper {
    pub fn with_table(&self, table_name: &'static str) -> Self {
        IPTablesWrapper {
            table_name,
            tables: self.tables.clone(),
        }
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub fn create_chain(&self, name: &str) -> IPTablesResult<()> {
        self.tables.new_chain(self.table_name, name)?;
        self.tables.append(self.table_name, name, "-j RETURN")?;

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub fn remove_chain(&self, name: &str) -> IPTablesResult<()> {
        self.tables.flush_chain(self.table_name, name)?;
        self.tables.delete_chain(self.table_name, name)?;

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub fn chain_exists(&self, chain: &str) -> IPTablesResult<bool> {
        Ok(self.tables.chain_exists(self.table_name, chain)?)
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub fn add_rule(&self, chain: &str, rule: &str) -> IPTablesResult<()> {
        self.tables
            .append(self.table_name, chain, rule)
            .map_err(From::from)
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub fn insert_rule(&self, chain: &str, rule: &str, index: i32) -> IPTablesResult<()> {
        self.tables
            .insert(self.table_name, chain, rule, index)
            .map_err(From::from)
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub fn list_rules(&self, chain: &str) -> IPTablesResult<Vec<String>> {
        self.tables.list(self.table_name, chain).map_err(From::from)
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub fn list_table(&self) -> IPTablesResult<Vec<String>> {
        self.tables.list_table(self.table_name).map_err(From::from)
    }

    #[tracing::instrument(level = Level::TRACE, ret, err)]
    pub fn remove_rule(&self, chain: &str, rule: &str) -> IPTablesResult<()> {
        self.tables
            .delete(self.table_name, chain, rule)
            .map_err(From::from)
    }
}

#[enum_dispatch(Redirect)]
enum Redirects {
    Ambient(AmbientRedirect),
    Standard(StandardRedirect),
    Mesh(MeshRedirect),
    FlushConnections(FlushConnections<Redirects>),
    PrerouteFallback(PreroutingRedirect),
    WithMeshExclusion(WithMeshExclusion<Redirects>),
}

/// Manages traffic redirection through explicit entrypoint cleanup and chain deletion on drop.
pub struct SafeIpTables {
    redirect: Redirects,
    ipt: Arc<IPTablesBackend>,
    chain_names: ChainNames,
}

/// Wrapper for using iptables. Managed chains are created on creation and deleted on drop.
/// Entrypoint rules are removed explicitly with [`Self::cleanup`] or [`Self::cleanup_verified`].
/// The way it works is that it adds a chain, then adds a rule to the chain that returns to the
/// original chain (fallback) and adds a rule in the "PREROUTING" table that jumps to the new chain.
/// Connections will then go PREROUTING -> OUR_CHAIN -> IF MATCH REDIRECT -> IF NOT MATCH FALLBACK
/// -> ORIGINAL_CHAIN
impl SafeIpTables {
    pub async fn create(
        ipt: IPTablesBackend,
        chain_names: &ChainNames,
        flush_connections: bool,
        pod_ips: Option<&str>,
        ipv6: bool,
        with_mesh_exclusion: bool,
    ) -> IPTablesResult<Self> {
        let ipt = Arc::new(ipt);

        let mut redirect = match MeshVendor::detect(ipt.as_ref())? {
            Some(vendor) => match &vendor {
                MeshVendor::IstioAmbient => {
                    Redirects::Ambient(AmbientRedirect::create(ipt.clone(), chain_names, pod_ips)?)
                }
                _ => Redirects::Mesh(MeshRedirect::create(
                    ipt.clone(),
                    chain_names,
                    vendor,
                    pod_ips,
                )?),
            },
            _ => {
                tracing::trace!(ipv6 = ipv6, "creating standard redirect");
                match StandardRedirect::create(ipt.clone(), chain_names, pod_ips) {
                    Err(err) => {
                        warn!("Unable to create StandardRedirect chain: {err}");

                        Redirects::PrerouteFallback(PreroutingRedirect::create(
                            ipt.clone(),
                            chain_names.prerouting.clone(),
                        )?)
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
            redirect = Redirects::WithMeshExclusion(WithMeshExclusion::create(
                ipt.clone(),
                &chain_names.exclude_from_mesh,
                Box::new(redirect),
            )?)
        }

        redirect.mount_entrypoint().await?;

        Ok(Self {
            redirect,
            ipt,
            chain_names: chain_names.clone(),
        })
    }

    /// List rules from previous mirrord agent that exist on the IP table
    #[tracing::instrument(level = Level::TRACE, skip(ipt, chain_names) ret, err)]
    pub async fn list_mirrord_rules<'a>(
        ipt: &'_ IPTablesBackend,
        chain_names: &'a ChainNames,
    ) -> IPTablesResult<impl Iterator<Item = String> + use<'a>> {
        let rules = ipt.list_table()?;

        Ok(rules.into_iter().filter(|rule| {
            [
                &chain_names.prerouting,
                &chain_names.mesh,
                &chain_names.standard,
                &chain_names.exclude_from_mesh,
            ]
            .iter()
            .any(|chain| rule.contains(chain.as_str()))
        }))
    }

    pub async fn load(
        ipt: IPTablesBackend,
        chain_names: &ChainNames,
        flush_connections: bool,
        with_mesh_exclusion: bool,
    ) -> IPTablesResult<Self> {
        let ipt = Arc::new(ipt);

        let mut redirect = match MeshVendor::detect(ipt.as_ref())? {
            Some(vendor) => match &vendor {
                MeshVendor::IstioAmbient => {
                    Redirects::Ambient(AmbientRedirect::load(ipt.clone(), chain_names)?)
                }
                _ => Redirects::Mesh(MeshRedirect::load(ipt.clone(), chain_names, vendor)?),
            },
            _ => match StandardRedirect::load(ipt.clone(), chain_names) {
                Err(err) => {
                    warn!("Unable to load StandardRedirect chain: {err}");

                    Redirects::PrerouteFallback(PreroutingRedirect::load(
                        ipt.clone(),
                        chain_names.prerouting.clone(),
                    )?)
                }
                Ok(standard) => Redirects::Standard(standard),
            },
        };

        if flush_connections {
            redirect = Redirects::FlushConnections(FlushConnections::load(Box::new(redirect))?)
        }

        // Should be always the last composed redirect because it handles the order internally.
        if with_mesh_exclusion {
            redirect = Redirects::WithMeshExclusion(WithMeshExclusion::load(
                ipt.clone(),
                &chain_names.exclude_from_mesh,
                Box::new(redirect),
            )?)
        }

        Ok(Self {
            redirect,
            ipt,
            chain_names: chain_names.clone(),
        })
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

    /// Runs [`Self::cleanup`], tolerating failures when no mirrord rules are left in the
    /// table anyway.
    ///
    /// `iptables -D` fails when the rule to delete does not exist, which happens when some
    /// external actor (e.g. a CNI or mesh component rebuilding the table) has already removed
    /// our rules. A cleanup failure is therefore verified against the actual table state:
    /// if no mirrord rules remain, the goal state holds and the error is discarded.
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub async fn cleanup_verified(self) -> IPTablesResult<()> {
        let Err(error) = self.cleanup().await else {
            return Ok(());
        };

        let Self {
            redirect,
            ipt,
            chain_names,
        } = self;
        drop(redirect);

        let no_rules_left = match Self::list_mirrord_rules(ipt.as_ref(), &chain_names).await {
            Ok(mut leftover_rules) => leftover_rules.next().is_none(),
            Err(..) => false,
        };

        if no_rules_left {
            warn!(
                %error,
                "iptables cleanup failed, but no mirrord rules are left in the table",
            );
            Ok(())
        } else {
            Err(error)
        }
    }

    pub fn exclusion(&self) -> Option<&MeshExclusion> {
        match &self.redirect {
            Redirects::WithMeshExclusion(redirect) => Some(redirect.exclusion()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::{eq, str};

    use crate::{ChainNames, MockIPTablesWrapper, SafeIpTables};

    #[tokio::test]
    async fn default() {
        let chain_names = ChainNames::legacy();
        let mut mock = MockIPTablesWrapper::new();

        mock.expect_list_rules()
            .with(eq("OUTPUT"))
            .returning(|_| Ok(vec![]));

        mock.expect_create_chain()
            .with(eq(chain_names.prerouting.clone()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(chain_names.prerouting.clone()),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_create_chain()
            .with(eq(chain_names.standard.clone()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(chain_names.standard.clone()),
                str::starts_with("-m owner --gid-owner"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(chain_names.standard.clone()),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_add_rule()
            .with(
                eq("PREROUTING"),
                eq(format!("-j {}", chain_names.prerouting)),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_add_rule()
            .with(eq("OUTPUT"), eq(format!("-j {}", chain_names.standard)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(chain_names.prerouting.clone()),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(chain_names.standard.clone()),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq("PREROUTING"),
                eq(format!("-j {}", chain_names.prerouting)),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(eq("OUTPUT"), eq(format!("-j {}", chain_names.standard)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(chain_names.prerouting.clone()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_chain()
            .with(eq(chain_names.standard.clone()))
            .times(1)
            .returning(|_| Ok(()));

        let ipt = SafeIpTables::create(mock, &chain_names, false, None, false, false)
            .await
            .expect("Create Failed");

        assert!(ipt.add_redirect(69, 420).await.is_ok());

        assert!(ipt.remove_redirect(69, 420).await.is_ok());

        assert!(ipt.cleanup().await.is_ok());
    }

    #[tokio::test]
    async fn linkerd() {
        let cn = ChainNames::legacy();
        let mut mock = MockIPTablesWrapper::new();

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
            .with(eq(cn.prerouting.clone()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(cn.prerouting.clone()),
                eq("-m multiport -p tcp ! --dports 22 -j RETURN"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_add_rule()
            .with(eq("PREROUTING"), eq(format!("-j {}", cn.prerouting)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_create_chain()
            .with(eq(cn.mesh.clone()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(cn.mesh.clone()),
                str::starts_with("-m owner --gid-owner"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_add_rule()
            .with(eq("OUTPUT"), eq(format!("-j {}", cn.mesh)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(cn.prerouting.clone()),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(cn.mesh.clone()),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(cn.prerouting.clone()),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(cn.mesh.clone()),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(eq("PREROUTING"), eq(format!("-j {}", cn.prerouting)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(cn.prerouting.clone()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_rule()
            .with(eq("OUTPUT"), eq(format!("-j {}", cn.mesh)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(cn.mesh.clone()))
            .times(1)
            .returning(|_| Ok(()));

        let ipt = SafeIpTables::create(mock, &cn, false, None, false, false)
            .await
            .expect("Create Failed");

        assert!(ipt.add_redirect(69, 420).await.is_ok());

        assert!(ipt.remove_redirect(69, 420).await.is_ok());

        assert!(ipt.cleanup().await.is_ok());
    }

    #[tokio::test]
    async fn with_mesh_exclusion() {
        let chain_names = ChainNames::legacy();
        let mut mock = MockIPTablesWrapper::new();

        mock.expect_list_rules()
            .with(eq("OUTPUT"))
            .returning(|_| Ok(vec![]));

        mock.expect_create_chain()
            .with(eq(chain_names.exclude_from_mesh.clone()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_create_chain()
            .with(eq(chain_names.prerouting.clone()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(chain_names.prerouting.clone()),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_create_chain()
            .with(eq(chain_names.standard.clone()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(chain_names.standard.clone()),
                str::starts_with("-m owner --gid-owner"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(chain_names.standard.clone()),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq("PREROUTING"),
                eq(format!("-j {}", chain_names.exclude_from_mesh)),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_add_rule()
            .with(
                eq("PREROUTING"),
                eq(format!("-j {}", chain_names.prerouting)),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_add_rule()
            .with(eq("OUTPUT"), eq(format!("-j {}", chain_names.standard)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(chain_names.prerouting.clone()),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq(chain_names.standard.clone()),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq("PREROUTING"),
                eq(format!("-j {}", chain_names.exclude_from_mesh)),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                eq("PREROUTING"),
                eq(format!("-j {}", chain_names.prerouting)),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(eq("OUTPUT"), eq(format!("-j {}", chain_names.standard)))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(chain_names.exclude_from_mesh.clone()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_chain()
            .with(eq(chain_names.prerouting.clone()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_chain()
            .with(eq(chain_names.standard.clone()))
            .times(1)
            .returning(|_| Ok(()));

        let ipt = SafeIpTables::create(mock, &chain_names, false, None, false, true)
            .await
            .expect("Create Failed");

        assert!(ipt.add_redirect(69, 420).await.is_ok());

        assert!(ipt.remove_redirect(69, 420).await.is_ok());

        assert!(ipt.cleanup().await.is_ok());
    }

    /// A fresh IP table, or one with only non-agent names, has no leftover rules for
    /// [`SafeIpTables::list_mirrord_rules`] to report.
    #[tokio::test]
    async fn pass_on_clean() {
        let chain_names = ChainNames::legacy();
        let mut mock = MockIPTablesWrapper::new();

        // clean table returns non-mirrord rules only
        mock.expect_list_table().with().times(1).returning(|| {
            Ok(vec![
                "-P PREROUTING ACCEPT".to_owned(),
                "-P INPUT ACCEPT".to_owned(),
                "-P OUTPUT ACCEPT".to_owned(),
                "-P POSTROUTING ACCEPT".to_owned(),
            ])
        });

        let leftover_rules_res = SafeIpTables::list_mirrord_rules(&mock, &chain_names).await;
        assert_eq!(
            leftover_rules_res.unwrap().count(),
            0,
            "Fresh IP table should successfully list table rules and list no existing mirrord rules"
        );
    }

    /// Chains with names used by the agent must be reported by
    /// [`SafeIpTables::list_mirrord_rules`] so stale redirection rules can be detected.
    #[tokio::test]
    async fn fail_on_dirty() {
        let chain_names = ChainNames::legacy();
        let mut mock = MockIPTablesWrapper::new();

        // dirty table returns non-mirrord rules, plus a leftover mirrord rule
        mock.expect_list_table().with().times(1).returning(|| {
            Ok(vec![
                "-P PREROUTING ACCEPT".to_owned(),
                "-P INPUT ACCEPT".to_owned(),
                "-P OUTPUT ACCEPT".to_owned(),
                "-P POSTROUTING ACCEPT".to_owned(),
                "-N MIRRORD_INPUT".to_owned(),
            ])
        });

        let leftover_rules_res = SafeIpTables::list_mirrord_rules(&mock, &chain_names).await;
        assert_eq!(
            leftover_rules_res.unwrap().count(),
            1,
            "Fresh IP table should successfully list table rules and list one existing mirrord rule"
        );
    }
}
