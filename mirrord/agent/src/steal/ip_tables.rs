use std::sync::{Arc, LazyLock};

use fancy_regex::Regex;
use mirrord_protocol::Port;

#[cfg(target_os = "linux")]
use crate::error::AgentError;
use crate::{
    error::Result,
    steal::ip_tables::{
        flush_connections::FlushConnections,
        mesh::{MeshRedirect, MeshVendor},
        redirect::{AsyncRedirect, PreroutingRedirect},
    },
};

mod chain;
mod flush_connections;
mod mesh;
mod redirect;

/// [`Regex`] used to select the `owner` rule from the list of `iptables` rules.
static UID_LOOKUP_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-m owner --uid-owner \d+").unwrap());

static SKIP_PORTS_LOOKUP_REGEX: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(r"-p tcp -m multiport --dports ([\d:,]+)").unwrap(),
        Regex::new(r"-p tcp -m tcp --dport ([\d:,]+)").unwrap(),
    ]
});

const IPTABLES_TABLE_NAME: &str = "nat";

#[cfg_attr(test, mockall::automock)]
pub trait IPTables {
    fn create_chain(&self, name: &str) -> Result<()>;
    fn remove_chain(&self, name: &str) -> Result<()>;

    fn add_rule(&self, chain: &str, rule: &str) -> Result<()>;
    fn insert_rule(&self, chain: &str, rule: &str, index: i32) -> Result<()>;
    fn list_rules(&self, chain: &str) -> Result<Vec<String>>;
    fn remove_rule(&self, chain: &str, rule: &str) -> Result<()>;
}

#[cfg(target_os = "linux")]
impl IPTables for iptables::IPTables {
    #[tracing::instrument(level = "trace", skip(self))]
    fn create_chain(&self, name: &str) -> Result<()> {
        self.new_chain(IPTABLES_TABLE_NAME, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;
        self.append(IPTABLES_TABLE_NAME, name, "-j RETURN")
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;

        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self))]
    fn remove_chain(&self, name: &str) -> Result<()> {
        self.flush_chain(IPTABLES_TABLE_NAME, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;
        self.delete_chain(IPTABLES_TABLE_NAME, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;

        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self))]
    fn add_rule(&self, chain: &str, rule: &str) -> Result<()> {
        self.append(IPTABLES_TABLE_NAME, chain, rule)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    #[tracing::instrument(level = "trace", skip(self))]
    fn insert_rule(&self, chain: &str, rule: &str, index: i32) -> Result<()> {
        self.insert(IPTABLES_TABLE_NAME, chain, rule, index)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    #[tracing::instrument(level = "trace", skip(self))]
    fn list_rules(&self, chain: &str) -> Result<Vec<String>> {
        self.list(IPTABLES_TABLE_NAME, chain)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    #[tracing::instrument(level = "trace", skip(self))]
    fn remove_rule(&self, chain: &str, rule: &str) -> Result<()> {
        self.delete(IPTABLES_TABLE_NAME, chain, rule)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }
}

/// Wrapper struct for IPTables so it flushes on drop.
pub(crate) struct SafeIpTables<IPT: IPTables> {
    inner: IPT,
    redirect: Arc<dyn AsyncRedirect>,
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
    pub(super) fn new(ipt: IPT, flush_connections: bool) -> Result<Self> {
        let redirect = if let Some(vendor) = MeshVendor::detect(&ipt)? {
            let meshed = MeshRedirect::create(&ipt, vendor)?;

            if flush_connections {
                Arc::new(FlushConnections::new(meshed)) as Arc<dyn AsyncRedirect>
            } else {
                Arc::new(meshed) as Arc<dyn AsyncRedirect>
            }
        } else {
            let prerouting = PreroutingRedirect::create(&ipt)?;

            if flush_connections {
                Arc::new(FlushConnections::new(prerouting)) as Arc<dyn AsyncRedirect>
            } else {
                Arc::new(prerouting) as Arc<dyn AsyncRedirect>
            }
        };

        Ok(Self {
            inner: ipt,
            redirect,
        })
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
        Ok(())
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
        Ok(())
    }
}

impl<IPT> Drop for SafeIpTables<IPT>
where
    IPT: IPTables,
{
    fn drop(&mut self) {}
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

        mock.expect_add_rule()
            .with(eq("PREROUTING"), str::starts_with("-j MIRRORD_INPUT_"))
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
            .with(eq("PREROUTING"), str::starts_with("-j MIRRORD_INPUT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_chain()
            .with(str::starts_with("MIRRORD_INPUT_"))
            .times(1)
            .returning(|_| Ok(()));

        let ipt = SafeIpTables::new(mock, false).expect("Create Failed");

        assert!(ipt.add_redirect(69, 420).await.is_ok());

        assert!(ipt.remove_redirect(69, 420).await.is_ok());
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

        let ipt = SafeIpTables::new(mock, false).expect("Create Failed");

        assert!(ipt.add_redirect(69, 420).await.is_ok());

        assert!(ipt.remove_redirect(69, 420).await.is_ok());
    }
}
