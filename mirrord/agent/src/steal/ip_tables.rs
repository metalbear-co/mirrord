use std::sync::LazyLock;

use fancy_regex::Regex;
use mirrord_protocol::Port;
use nix::unistd::getgid;
use rand::distributions::{Alphanumeric, DistString};
use tokio::process::Command;
use tracing::warn;

use crate::error::{AgentError, Result};

pub(crate) static MIRRORD_IPTABLE_CHAIN_ENV: &str = "MIRRORD_IPTABLE_CHAIN_NAME";

static UID_LOOKUP_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-m owner --uid-owner \d+").unwrap());

#[cfg_attr(test, mockall::automock)]
pub(crate) trait IPTables {
    fn create_chain(&self, name: &str) -> Result<()>;
    fn remove_chain(&self, name: &str) -> Result<()>;

    fn add_rule(&self, chain: &str, rule: &str) -> Result<()>;
    fn insert_rule(&self, chain: &str, rule: &str, index: i32) -> Result<()>;
    fn list_rules(&self, chain: &str) -> Result<Vec<String>>;
    fn remove_rule(&self, chain: &str, rule: &str) -> Result<()>;
}

impl IPTables for iptables::IPTables {
    fn create_chain(&self, name: &str) -> Result<()> {
        self.new_chain(IPTABLES_TABLE_NAME, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;
        self.append(IPTABLES_TABLE_NAME, name, "-j RETURN")
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;

        Ok(())
    }

    fn remove_chain(&self, name: &str) -> Result<()> {
        self.flush_chain(IPTABLES_TABLE_NAME, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;
        self.delete_chain(IPTABLES_TABLE_NAME, name)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))?;

        Ok(())
    }

    fn add_rule(&self, chain: &str, rule: &str) -> Result<()> {
        self.append(IPTABLES_TABLE_NAME, chain, rule)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    fn insert_rule(&self, chain: &str, rule: &str, index: i32) -> Result<()> {
        self.insert(IPTABLES_TABLE_NAME, chain, rule, index)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    fn list_rules(&self, chain: &str) -> Result<Vec<String>> {
        self.list(IPTABLES_TABLE_NAME, chain)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }

    fn remove_rule(&self, chain: &str, rule: &str) -> Result<()> {
        self.delete(IPTABLES_TABLE_NAME, chain, rule)
            .map_err(|e| AgentError::IPTablesError(e.to_string()))
    }
}

/// Wrapper struct for IPTables so it flushes on drop.
pub(crate) struct SafeIpTables<IPT: IPTables> {
    inner: IPT,
    chain_name: String,
    formatter: IPTableFormatter,
    flush_connections: bool,
}

const IPTABLES_TABLE_NAME: &str = "nat";
/// Wrapper for using iptables. This creates a a new chain on creation and deletes it on drop.
/// The way it works is that it adds a chain, then adds a rule to the chain that returns to the
/// original chain (fallback) and adds a rule in the "PREROUTING" table that jumps to the new chain.
/// Connections will go then PREROUTING -> OUR_CHAIN -> IF MATCH REDIRECT -> IF NOT MATCH FALLBACK
/// -> ORIGINAL_CHAIN
impl<IPT> SafeIpTables<IPT>
where
    IPT: IPTables,
{
    pub(super) fn new(ipt: IPT, flush_connections: bool) -> Result<Self> {
        let formatter = IPTableFormatter::detect(&ipt)?;

        let chain_name = Self::get_chain_name();

        ipt.create_chain(&chain_name)?;

        ipt.add_rule(formatter.entrypoint(), &format!("-j {}", &chain_name))?;

        Ok(Self {
            inner: ipt,
            chain_name,
            formatter,
            flush_connections,
        })
    }

    pub(crate) fn get_chain_name() -> String {
        std::env::var(MIRRORD_IPTABLE_CHAIN_ENV).unwrap_or_else(|_| {
            format!(
                "MIRRORD_REDIRECT_{}",
                Alphanumeric.sample_string(&mut rand::thread_rng(), 5)
            )
        })
    }

    pub(crate) fn remove_chain(
        ipt: &IPT,
        formatter: &IPTableFormatter,
        chain_name: &str,
    ) -> Result<()> {
        ipt.remove_rule(formatter.entrypoint(), &format!("-j {chain_name}"))?;

        ipt.remove_chain(chain_name)?;

        Ok(())
    }

    /// Helper function that lists all the iptables' rules belonging to [`Self::chain_name`].
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn list_rules(&self) -> Result<Vec<String>> {
        self.inner.list_rules(&self.chain_name)
    }

    /// Adds the redirect rule to iptables.
    ///
    /// Used to redirect packets when mirrord incoming feature is set to `steal`.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn add_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.inner.insert_rule(
            &self.chain_name,
            &self.formatter.redirect_rule(redirected_port, target_port),
            1,
        )
    }

    /// Removes the redirect rule from iptables.
    ///
    /// Stops redirecting packets when mirrord incoming feature is set to `steal`, and there are no
    /// more subscribers on `target_port`.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.inner.remove_rule(
            &self.chain_name,
            &self.formatter.redirect_rule(redirected_port, target_port),
        )
    }

    /// Adds a `RETURN` rule based on `gid` to iptables.
    ///
    /// When the mirrord incoming feature is set to `steal`, and we're using a filter (instead of
    /// stealing every packet), we need this rule to avoid stealing our own packets, that were sent
    /// to their original destinations.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn add_bypass_own_packets(&self) -> Result<()> {
        if let Some(rule) = self.formatter.bypass_own_packets_rule() {
            self.inner.insert_rule(&self.chain_name, &rule, 1)
        } else {
            Ok(())
        }
    }

    /// Removes the `RETURN` bypass rule from iptables.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn remove_bypass_own_packets(&self) -> Result<()> {
        if let Some(rule) = self.formatter.bypass_own_packets_rule() {
            self.inner.remove_rule(&self.chain_name, &rule)
        } else {
            Ok(())
        }
    }

    /// Adds port redirection, and bypass gid packets from iptables.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) async fn add_stealer_iptables_rules(
        &self,
        redirected_port: Port,
        target_port: Port,
    ) -> Result<()> {
        self.add_redirect(redirected_port, target_port)
            .and_then(|_| self.add_bypass_own_packets())?;

        if self.flush_connections {
            let conntrack = Command::new("conntrack")
                .args([
                    "--delete",
                    "--proto",
                    "tcp",
                    "--orig-port-dst",
                    &target_port.to_string(),
                ])
                .output()
                .await?;

            if !conntrack.status.success() && conntrack.status.code() != Some(256) {
                warn!("`conntrack` output is {conntrack:#?}");
            }
        }

        Ok(())
    }

    /// Removes port redirection, and bypass gid packets from iptables.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn remove_stealer_iptables_rules(
        &self,
        redirected_port: Port,
        target_port: Port,
    ) -> Result<()> {
        self.remove_redirect(redirected_port, target_port)
            .and_then(|_| self.remove_bypass_own_packets())
    }
}

impl<IPT> Drop for SafeIpTables<IPT>
where
    IPT: IPTables,
{
    fn drop(&mut self) {
        Self::remove_chain(&self.inner, &self.formatter, &self.chain_name).unwrap();
    }
}

#[derive(Debug)]
pub(crate) enum IPTableFormatter {
    Normal,
    Mesh(String),
}

impl IPTableFormatter {
    const MESH_OUTPUTS: [&'static str; 2] = ["-j PROXY_INIT_OUTPUT", "-j ISTIO_OUTPUT"];
    const MESH_NAMES: [&'static str; 2] = ["PROXY_INIT_OUTPUT", "ISTIO_OUTPUT"];

    #[tracing::instrument(level = "trace", skip_all)]
    pub(crate) fn detect<IPT: IPTables>(ipt: &IPT) -> Result<Self> {
        let output = ipt.list_rules("OUTPUT")?;

        if let Some(mesh_ipt_chain) = output.iter().find_map(|rule| {
            IPTableFormatter::MESH_OUTPUTS
                .iter()
                .enumerate()
                .find_map(|(index, mesh_output)| {
                    rule.contains(mesh_output)
                        .then_some(IPTableFormatter::MESH_NAMES[index])
                })
        }) {
            let filter = ipt
                .list_rules(mesh_ipt_chain)?
                .iter()
                .find_map(|rule| UID_LOOKUP_REGEX.find(rule).ok().flatten())
                .map(|m| m.as_str().to_owned());

            Ok(IPTableFormatter::Mesh(
                filter.unwrap_or_else(|| "-o lo".to_owned()),
            ))
        } else {
            Ok(IPTableFormatter::Normal)
        }
    }

    fn entrypoint(&self) -> &str {
        match self {
            IPTableFormatter::Normal => "PREROUTING",
            IPTableFormatter::Mesh(_) => "OUTPUT",
        }
    }

    #[tracing::instrument(level = "trace", ret)]
    fn redirect_rule(&self, redirected_port: Port, target_port: Port) -> String {
        let redirect_rule =
            format!("-m tcp -p tcp --dport {redirected_port} -j REDIRECT --to-ports {target_port}");

        match self {
            IPTableFormatter::Normal => redirect_rule,
            IPTableFormatter::Mesh(filter) => {
                format!("{filter} {redirect_rule}")
            }
        }
    }

    fn bypass_own_packets_rule(&self) -> Option<String> {
        match self {
            IPTableFormatter::Normal => None,
            IPTableFormatter::Mesh(_) => {
                let gid = getgid();
                Some(format!("-m owner --gid-owner {gid} -p tcp -j RETURN"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::*;

    use super::*;

    #[test]
    fn default() {
        let mut mock = MockIPTables::new();

        mock.expect_list_rules()
            .with(eq("OUTPUT"))
            .returning(|_| Ok(vec![]));

        mock.expect_create_chain()
            .with(str::starts_with("MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_chain()
            .with(str::starts_with("MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_add_rule()
            .with(eq("PREROUTING"), str::starts_with("-j MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                str::starts_with("MIRRORD_REDIRECT_"),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_remove_rule()
            .with(eq("PREROUTING"), str::starts_with("-j MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                str::starts_with("MIRRORD_REDIRECT_"),
                eq("-m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        let ipt = SafeIpTables::new(mock, false).expect("Create Failed");

        assert!(ipt.add_redirect(69, 420).is_ok());

        assert!(ipt.remove_redirect(69, 420).is_ok());
    }

    #[test]
    fn linkerd() {
        let mut mock = MockIPTables::new();

        mock.expect_list_rules()
            .with(eq("OUTPUT"))
            .returning(|_| Ok(vec!["-j PROXY_INIT_OUTPUT".to_owned()]));

        mock.expect_list_rules()
            .with(eq("PROXY_INIT_OUTPUT"))
            .returning(|_| Ok(vec![
                "-N PROXY_INIT_OUTPUT".to_owned(),
                "-A PROXY_INIT_OUTPUT -m owner --uid-owner 2102 -m comment --comment \"proxy-init/ignore-proxy-user-id/1676542558\" -j RETURN".to_owned(),
                "-A PROXY_INIT_OUTPUT -o lo -m comment --comment \"proxy-init/ignore-loopback/1676542558\" -j RETURN".to_owned(),
            ]));

        mock.expect_create_chain()
            .with(str::starts_with("MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_chain()
            .with(str::starts_with("MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_add_rule()
            .with(eq("OUTPUT"), str::starts_with("-j MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                str::starts_with("MIRRORD_REDIRECT_"),
                eq("-m owner --uid-owner 2102 -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_remove_rule()
            .with(eq("OUTPUT"), str::starts_with("-j MIRRORD_REDIRECT_"))
            .times(1)
            .returning(|_, _| Ok(()));

        mock.expect_remove_rule()
            .with(
                str::starts_with("MIRRORD_REDIRECT_"),
                eq("-m owner --uid-owner 2102 -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        let ipt = SafeIpTables::new(mock, false).expect("Create Failed");

        assert!(ipt.add_redirect(69, 420).is_ok());

        assert!(ipt.remove_redirect(69, 420).is_ok());
    }
}
