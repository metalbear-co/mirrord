use std::sync::{
    atomic::{AtomicI32, Ordering},
    LazyLock,
};

use fancy_regex::Regex;
use mirrord_protocol::Port;
use nix::unistd::getgid;
use rand::distributions::{Alphanumeric, DistString};
use tokio::process::Command;
use tracing::warn;

#[cfg(target_os = "linux")]
use crate::error::AgentError;
use crate::error::Result;

pub(crate) static MIRRORD_IPTABLE_PREROUTING_ENV: &str = "MIRRORD_IPTABLE_PREROUTING_NAME";
pub(crate) static MIRRORD_IPTABLE_OUTPUT_ENV: &str = "MIRRORD_IPTABLE_OUTPUT_NAME";

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
pub(crate) trait IPTables {
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
    chains: Vec<IpTableChain>,
    flush_connections: bool,
}

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

        let chains = formatter.chains();

        for chain in &chains {
            ipt.create_chain(&chain.name)?;

            if chain.entrypoint_name == "OUTPUT" && let Some(bypass) = formatter.bypass_own_packets_rule() {
                ipt.insert_rule(
                    &chain.name,
                    &bypass,
                    chain.rule_index.fetch_add(1, Ordering::Relaxed),
                )?;
            }

            for (entrypoint, entrypoint_rule) in chain.entrypoint() {
                if entrypoint == chain.name {
                    ipt.insert_rule(
                        entrypoint,
                        &entrypoint_rule,
                        chain.rule_index.fetch_add(1, Ordering::Relaxed),
                    )?;
                } else {
                    ipt.add_rule(entrypoint, &entrypoint_rule)?;
                }
            }
        }

        Ok(Self {
            inner: ipt,
            chains,
            flush_connections,
        })
    }

    /// Adds the redirect rule to iptables.
    ///
    /// Used to redirect packets when mirrord incoming feature is set to `steal`.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn add_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        for chain in &self.chains {
            self.inner.insert_rule(
                &chain.name,
                &chain.redirect(redirected_port, target_port),
                chain.rule_index.fetch_add(1, Ordering::Relaxed),
            )?;
        }

        Ok(())
    }

    /// Removes the redirect rule from iptables.
    ///
    /// Stops redirecting packets when mirrord incoming feature is set to `steal`, and there are no
    /// more subscribers on `target_port`.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        for chain in &self.chains {
            self.inner
                .remove_rule(&chain.name, &chain.redirect(redirected_port, target_port))?;

            chain.rule_index.fetch_sub(1, Ordering::Relaxed);
        }

        Ok(())
    }

    /// Adds port redirection, and bypass gid packets from iptables.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) async fn add_stealer_iptables_rules(
        &self,
        redirected_port: Port,
        target_port: Port,
    ) -> Result<()> {
        self.add_redirect(redirected_port, target_port)?;

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
}

impl<IPT> Drop for SafeIpTables<IPT>
where
    IPT: IPTables,
{
    fn drop(&mut self) {
        for chain in &self.chains {
            let _ = chain.remove(&self.inner);
        }
    }
}

#[derive(Debug)]
pub(crate) enum IPTableFormatter {
    Normal,
    Mesh {
        own_packet_filter: Option<String>,
        skipped_ports: Vec<String>,
    },
}

impl IPTableFormatter {
    const MESH_ENTRYPOINTS: [&'static str; 2] = ["-j PROXY_INIT_OUTPUT", "-j ISTIO_OUTPUT"];
    const MESH_INPUT_NAMES: [&'static str; 2] = ["PROXY_INIT_REDIRECT", "ISTIO_INBOUND"];
    const MESH_OUTPUT_NAMES: [&'static str; 2] = ["PROXY_INIT_OUTPUT", "ISTIO_OUTPUT"];

    #[tracing::instrument(level = "trace", skip_all)]
    pub(crate) fn detect<IPT: IPTables>(ipt: &IPT) -> Result<Self> {
        let output = ipt.list_rules("OUTPUT")?;

        if let Some(mesh_index) = output.iter().find_map(|rule| {
            IPTableFormatter::MESH_ENTRYPOINTS
                .iter()
                .enumerate()
                .find_map(|(index, mesh_output)| rule.contains(mesh_output).then_some(index))
        }) {
            // We extract --uid-owner value from the mesh's rules to get messages only from them
            // and not other processes sendning messages from localhost like healthprobe for
            // grpc. This to more closely match behavior with non meshed
            // services
            let own_packet_filter = ipt
                    .list_rules(Self::MESH_OUTPUT_NAMES[mesh_index])?
                    .iter()
                    .find_map(|rule| UID_LOOKUP_REGEX.find(rule).ok().flatten())
                    .map(|m| format!("-o lo {}", m.as_str())).or_else(|| {
                        warn!("Couldn't find --uid-owner of meshed chain {:?} falling back on \"-o lo\" rule", Self::MESH_OUTPUT_NAMES[mesh_index]);

                        Some("-o lo".to_owned())
                    });

            let skipped_ports = ipt
                .list_rules(IPTableFormatter::MESH_INPUT_NAMES[mesh_index])?
                .iter()
                .filter_map(|rule| {
                    SKIP_PORTS_LOOKUP_REGEX[mesh_index]
                        .captures(rule)
                        .ok()
                        .flatten()
                        .and_then(|capture| capture.get(1))
                })
                .map(|m| m.as_str().to_string())
                .collect();

            Ok(IPTableFormatter::Mesh {
                own_packet_filter,
                skipped_ports,
            })
        } else {
            Ok(IPTableFormatter::Normal)
        }
    }

    pub(crate) fn chains(&self) -> Vec<IpTableChain> {
        match self {
            IPTableFormatter::Normal => vec![IpTableChain::prerouting(vec![])],
            IPTableFormatter::Mesh {
                own_packet_filter,
                skipped_ports,
            } => vec![
                IpTableChain::prerouting(skipped_ports.clone()),
                IpTableChain::output(own_packet_filter.clone()),
            ],
        }
    }

    /// Adds a `RETURN` rule based on `gid` to iptables.
    ///
    /// When the mirrord incoming feature is set to `steal`, and we're using a filter (instead of
    /// stealing every packet), we need this rule to avoid stealing our own packets, that were sent
    /// to their original destinations.
    fn bypass_own_packets_rule(&self) -> Option<String> {
        match self {
            IPTableFormatter::Normal => None,
            IPTableFormatter::Mesh { .. } => {
                let gid = getgid();
                Some(format!("-m owner --gid-owner {gid} -p tcp -j RETURN"))
            }
        }
    }
}

#[derive(Debug)]
pub struct IpTableChain {
    entrypoint_name: &'static str,
    name: String,
    redirect_filter: Option<String>,
    rule_index: AtomicI32,
    skipped_ports: Vec<String>,
}

impl IpTableChain {
    fn prerouting(skipped_ports: Vec<String>) -> Self {
        let chain_name = Self::prerouting_name();

        IpTableChain {
            entrypoint_name: "PREROUTING",
            name: chain_name,
            redirect_filter: None,
            rule_index: AtomicI32::from(1),
            skipped_ports,
        }
    }

    fn output(redirect_filter: Option<String>) -> Self {
        let chain_name = Self::output_name();

        IpTableChain {
            entrypoint_name: "OUTPUT",
            name: chain_name,
            redirect_filter,
            rule_index: AtomicI32::from(1),
            skipped_ports: vec![],
        }
    }

    pub(crate) fn prerouting_name() -> String {
        std::env::var(MIRRORD_IPTABLE_PREROUTING_ENV).unwrap_or_else(|_| {
            format!(
                "MIRRORD_INPUT_{}",
                Alphanumeric.sample_string(&mut rand::thread_rng(), 5)
            )
        })
    }

    pub(crate) fn output_name() -> String {
        std::env::var(MIRRORD_IPTABLE_OUTPUT_ENV).unwrap_or_else(|_| {
            format!(
                "MIRRORD_OUTPUT_{}",
                Alphanumeric.sample_string(&mut rand::thread_rng(), 5)
            )
        })
    }

    fn entrypoint(&self) -> Vec<(&str, String)> {
        std::iter::once((self.entrypoint_name, format!("-j {}", self.name)))
            .chain(self.skipped_ports.iter().map(|port| {
                (
                    self.name.as_str(),
                    format!("-m multiport -p tcp ! --dports {port} -j RETURN"),
                )
            }))
            .collect()
    }

    fn redirect(&self, redirected_port: Port, target_port: Port) -> String {
        let redirect_rule =
            format!("-m tcp -p tcp --dport {redirected_port} -j REDIRECT --to-ports {target_port}");

        match &self.redirect_filter {
            Some(filter) => format!("{filter} {redirect_rule}"),
            None => redirect_rule,
        }
    }

    pub(crate) fn remove<IPT: IPTables>(&self, ipt: &IPT) -> Result<()> {
        ipt.remove_rule(self.entrypoint_name, &format!("-j {}", self.name))?;

        ipt.remove_chain(&self.name)
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
                eq("-p tcp -m multiport ! --dports 22 -j RETURN"),
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

        assert!(ipt.add_redirect(69, 420).is_ok());

        assert!(ipt.remove_redirect(69, 420).is_ok());
    }
}
