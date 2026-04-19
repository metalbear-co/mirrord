use std::{ops::Not, sync::Arc};

use async_trait::async_trait;
use tracing::warn;

use crate::{
    ChainNames, IPTables, error::IPTablesResult, output::OutputRedirect,
    prerouting::PreroutingRedirect, redirect::Redirect,
};

/// Sentinel chain used to coordinate `/proc/sys/net/ipv4/conf/all/route_localnet`
/// across multiple mirrord agents sharing a pod's network namespace.
///
/// Istio ambient mode requires `route_localnet=1`, but we must restore the pre-mirrord
/// value when the last agent leaves. We are basically using this iptables chain with its rules as a
/// shared resource to store the original `route_localnet` value and refcounts per agent.
///
/// The chain holds no-op rules (protocol 255, `-j RETURN`) that never match real traffic.
/// They exist only as data: their comments are the coordination state.
const ROUTE_LOCALNET_CHAIN: &str = "MRD_ROUTE_LOCALNET";

/// Prefix for the rule that stores the pre-mirrord `route_localnet` value in a comment.
const ORIGINAL_PREFIX: &str = "mirrord-localnet-original-";

/// Prefix for per-agent refcount rules. The suffix is the agent's unique chain id;
/// when no `ref` rules remain, the last agent is responsible for restoring the original
/// value and tearing the chain down.
const REF_PREFIX: &str = "mirrord-localnet-ref-";

pub struct AmbientRedirect<IPT: IPTables> {
    prerouting: PreroutingRedirect<IPT>,
    output: OutputRedirect<true, IPT>,
    ipt: Arc<IPT>,
    /// Unique id for this agent, used as the suffix of our refcount rule so we can
    /// identify and remove our own ref on shutdown without disturbing other agents.
    agent_id: String,
}

impl<IPT> AmbientRedirect<IPT>
where
    IPT: IPTables,
{
    pub fn create(
        ipt: Arc<IPT>,
        chain_names: &ChainNames,
        pod_ips: Option<&str>,
    ) -> IPTablesResult<Self> {
        let prerouting = PreroutingRedirect::create(ipt.clone(), chain_names.prerouting.clone())?;
        let output = OutputRedirect::create(ipt.clone(), chain_names.mesh.clone(), pod_ips)?;

        Ok(AmbientRedirect {
            prerouting,
            output,
            ipt,
            agent_id: chain_names.prerouting.clone(),
        })
    }

    pub fn load(ipt: Arc<IPT>, chain_names: &ChainNames) -> IPTablesResult<Self> {
        let prerouting = PreroutingRedirect::load(ipt.clone(), chain_names.prerouting.clone())?;
        let output = OutputRedirect::load(ipt.clone(), chain_names.mesh.clone())?;

        Ok(AmbientRedirect {
            prerouting,
            output,
            ipt,
            agent_id: chain_names.prerouting.clone(),
        })
    }

    fn original_rule(value: &str) -> String {
        format!("-p 255 -m comment --comment {ORIGINAL_PREFIX}{value} -j RETURN")
    }

    fn ref_rule(agent_id: &str) -> String {
        format!("-p 255 -m comment --comment {REF_PREFIX}{agent_id} -j RETURN")
    }

    /// Ensures the sentinel chain exists, captures the pre-mirrord `route_localnet`
    /// value the first time any agent arrives, and registers this agent's ref.
    async fn register_route_localnet(&self) -> IPTablesResult<()> {
        if self.ipt.chain_exists(ROUTE_LOCALNET_CHAIN)?.not() {
            let original = tokio::fs::read_to_string("/proc/sys/net/ipv4/conf/all/route_localnet")
                .await
                .map(|v| v.trim().to_owned())
                .unwrap_or_else(|_| "0".to_owned());

            match self.ipt.create_chain(ROUTE_LOCALNET_CHAIN) {
                Ok(()) => {
                    self.ipt
                        .add_rule(ROUTE_LOCALNET_CHAIN, &Self::original_rule(&original))?;
                }
                Err(err) => {
                    // Failing to create the chain might mean that another agent beat us to it, so
                    // we only error out if the chain still doesn't exist.
                    if self
                        .ipt
                        .chain_exists(ROUTE_LOCALNET_CHAIN)
                        .unwrap_or(false)
                        .not()
                    {
                        return Err(err);
                    }
                }
            }
        }

        self.ipt
            .add_rule(ROUTE_LOCALNET_CHAIN, &Self::ref_rule(&self.agent_id))?;

        Ok(())
    }

    /// Removes this agent's ref. If no other refs remain, restores the stashed
    /// original `route_localnet` value and deletes the sentinel chain.
    async fn deregister_route_localnet(&self) -> IPTablesResult<()> {
        if self.ipt.chain_exists(ROUTE_LOCALNET_CHAIN)?.not() {
            warn!(
                "route_localnet sentinel chain missing at cleanup; skipping restore. \
                 Another agent may have removed it prematurely."
            );
            return Ok(());
        }

        self.ipt
            .remove_rule(ROUTE_LOCALNET_CHAIN, &Self::ref_rule(&self.agent_id))?;

        let rules = self.ipt.list_rules(ROUTE_LOCALNET_CHAIN)?;
        let other_refs_remain = rules
            .iter()
            .any(|rule| rule.contains(REF_PREFIX) && rule.contains(&self.agent_id).not());

        if other_refs_remain {
            return Ok(());
        }

        let original = rules
            .iter()
            .find_map(|rule| {
                rule.split_whitespace()
                    .find_map(|parts| parts.strip_prefix(ORIGINAL_PREFIX))
            })
            .map(str::to_owned)
            .unwrap_or_else(|| {
                warn!("route_localnet sentinel missing `original` rule; defaulting to 0");
                "0".to_owned()
            });

        tokio::fs::write(
            "/proc/sys/net/ipv4/conf/all/route_localnet",
            original.as_bytes(),
        )
        .await?;

        // Best-effort: a concurrent last-out in another agent may have removed the chain
        // between our list and this call. That's fine — the state we care about (`/proc`)
        // is already restored.
        if let Err(err) = self.ipt.remove_chain(ROUTE_LOCALNET_CHAIN) {
            warn!(
                %err,
                "Failed to remove route_localnet sentinel chain; another agent may have \
                 removed it concurrently."
            );
        }

        Ok(())
    }
}

#[async_trait]
impl<IPT> Redirect for AmbientRedirect<IPT>
where
    IPT: IPTables + Send + Sync,
{
    async fn mount_entrypoint(&self) -> IPTablesResult<()> {
        self.register_route_localnet().await?;
        // To prevent a race, we write `1` unconditionally on every mount.
        tokio::fs::write("/proc/sys/net/ipv4/conf/all/route_localnet", b"1").await?;

        self.prerouting.mount_entrypoint().await?;
        self.output.mount_entrypoint().await?;

        Ok(())
    }

    async fn unmount_entrypoint(&self) -> IPTablesResult<()> {
        // Don't fail early, so that we delete the second part even if the first part does not
        // exist and its deletion therefore fails.
        let prerouting_res = self.prerouting.unmount_entrypoint().await;
        let output_res = self.output.unmount_entrypoint().await;
        let sentinel_res = self.deregister_route_localnet().await;

        prerouting_res.and(output_res).and(sentinel_res)
    }

    async fn add_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()> {
        self.prerouting
            .add_redirect(redirected_port, target_port)
            .await?;
        self.output
            .add_redirect(redirected_port, target_port)
            .await?;

        Ok(())
    }

    async fn remove_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()> {
        self.prerouting
            .remove_redirect(redirected_port, target_port)
            .await?;
        self.output
            .remove_redirect(redirected_port, target_port)
            .await?;

        Ok(())
    }
}
