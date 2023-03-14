use std::sync::{Arc, LazyLock};

use fancy_regex::Regex;
use mirrord_protocol::Port;
use nix::unistd::getgid;
use rand::distributions::{Alphanumeric, DistString};
use tracing::warn;

use crate::{
    error::Result,
    steal::ip_tables::{
        chain::IPTableChain,
        redirect::{PreroutingRedirect, Redirect},
        IPTables,
    },
};

/// [`Regex`] used to select the `owner` rule from the list of `iptables` rules.
static UID_LOOKUP_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-m owner --uid-owner \d+").unwrap());

static LINKERD_SKIP_PORTS_LOOKUP_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-p tcp -m multiport --dports ([\d:,]+)").unwrap());

static ISTIO_SKIP_PORTS_LOOKUP_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-p tcp -m tcp --dport ([\d:,]+)").unwrap());

pub static IPTABLE_MESH_ENV: &str = "MIRRORD_IPTABLE_MESH_NAME";
pub static IPTABLE_MESH: LazyLock<String> = LazyLock::new(|| {
    std::env::var(IPTABLE_MESH_ENV).unwrap_or_else(|_| {
        format!(
            "MIRRORD_OUTPUT_{}",
            Alphanumeric.sample_string(&mut rand::thread_rng(), 5)
        )
    })
});

pub struct MeshRedirect<IPT> {
    preroute: PreroutingRedirect<IPT>,
    managed: IPTableChain<IPT>,
    own_packet_filter: String,
}

impl<IPT> MeshRedirect<IPT>
where
    IPT: IPTables,
{
    pub fn create(ipt: Arc<IPT>, vendor: MeshVendor) -> Result<Self> {
        let preroute = PreroutingRedirect::create(ipt.clone())?;
        let own_packet_filter = Self::get_own_packet_filter(&ipt, &vendor)?;

        for port in Self::get_skip_ports(&ipt, &vendor)? {
            preroute.add_rule(&format!("-m multiport -p tcp ! --dports {port} -j RETURN"))?;
        }

        let managed = IPTableChain::create(ipt, IPTABLE_MESH.to_string())?;

        let gid = getgid();
        managed.add_rule(&format!("-m owner --gid-owner {gid} -p tcp -j RETURN"))?;

        Ok(MeshRedirect {
            preroute,
            managed,
            own_packet_filter,
        })
    }

    fn get_own_packet_filter(ipt: &IPT, vendor: &MeshVendor) -> Result<String> {
        let chain_name = vendor.output_chain();

        let own_packet_filter = ipt
            .list_rules(chain_name)?
            .iter()
            .find_map(|rule| UID_LOOKUP_REGEX.find(rule).ok().flatten())
            .map(|m| format!("-o lo {}", m.as_str()))
            .unwrap_or_else(|| {
                warn!(
                    "Couldn't find --uid-owner of meshed chain {chain_name:?} falling back on \"-o lo\" rule",
                );

                "-o lo".to_owned()
            });

        Ok(own_packet_filter)
    }

    fn get_skip_ports(ipt: &IPT, vendor: &MeshVendor) -> Result<Vec<String>> {
        let lookup_regex = vendor.skip_ports_regex();

        let skipped_ports = ipt
            .list_rules(vendor.input_chain())?
            .iter()
            .filter_map(|rule| {
                lookup_regex
                    .captures(rule)
                    .ok()
                    .flatten()
                    .and_then(|capture| capture.get(1))
            })
            .map(|m| m.as_str().to_string())
            .collect();

        Ok(skipped_ports)
    }
}

impl<IPT> Redirect for MeshRedirect<IPT>
where
    IPT: IPTables,
{
    const ENTRYPOINT: &'static str = "OUTPUT";

    fn mount_entrypoint(&self) -> Result<()> {
        self.preroute.mount_entrypoint()?;

        self.managed.inner().add_rule(
            Self::ENTRYPOINT,
            &format!("-j {}", self.managed.chain_name()),
        )?;

        Ok(())
    }

    fn add_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.preroute.add_redirect(redirected_port, target_port)?;

        let redirect_rule = format!(
            "{} -m tcp -p tcp --dport {redirected_port} -j REDIRECT --to-ports {target_port}",
            self.own_packet_filter
        );

        self.managed.add_rule(&redirect_rule)?;

        Ok(())
    }

    fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        self.preroute
            .remove_redirect(redirected_port, target_port)?;

        let redirect_rule = format!(
            "{} -m tcp -p tcp --dport {redirected_port} -j REDIRECT --to-ports {target_port}",
            self.own_packet_filter
        );

        self.managed.remove_rule(&redirect_rule)?;

        Ok(())
    }
}

pub enum MeshVendor {
    Linkerd,
    Istio,
}

impl MeshVendor {
    pub fn detect<IPT: IPTables>(ipt: &IPT) -> Result<Option<Self>> {
        let output = ipt.list_rules("OUTPUT")?;

        Ok(output.iter().find_map(|rule| {
            if rule.contains("-j PROXY_INIT_OUTPUT") {
                Some(MeshVendor::Linkerd)
            } else if rule.contains("-j ISTIO_OUTPUT") {
                Some(MeshVendor::Istio)
            } else {
                None
            }
        }))
    }

    fn input_chain(&self) -> &str {
        match self {
            MeshVendor::Linkerd => "PROXY_INIT_REDIRECT",
            MeshVendor::Istio => "ISTIO_INBOUND",
        }
    }

    fn output_chain(&self) -> &str {
        match self {
            MeshVendor::Linkerd => "PROXY_INIT_OUTPUT",
            MeshVendor::Istio => "ISTIO_OUTPUT",
        }
    }

    fn skip_ports_regex(&self) -> &Regex {
        match self {
            MeshVendor::Linkerd => &LINKERD_SKIP_PORTS_LOOKUP_REGEX,
            MeshVendor::Istio => &ISTIO_SKIP_PORTS_LOOKUP_REGEX,
        }
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::*;

    use super::*;
    use crate::steal::ip_tables::{redirect::IPTABLE_PREROUTING, MockIPTables};

    fn create_mesh_list_values(mock: &mut MockIPTables) {
        mock.expect_list_rules()
            .with(eq("OUTPUT"))
            .returning(|_| Ok(vec!["-j PROXY_INIT_OUTPUT".to_owned()]));

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
    }

    #[test]
    fn add_redirect() {
        let gid = getgid();
        let mut mock = MockIPTables::new();

        create_mesh_list_values(&mut mock);

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

        mock.expect_create_chain()
            .with(eq(IPTABLE_MESH.as_str()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_MESH.as_str()),
                eq(format!("-m owner --gid-owner {gid} -p tcp -j RETURN")),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_MESH.as_str()),
                eq("-o lo -m owner --uid-owner 2102 -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        let prerouting =
            MeshRedirect::create(Arc::new(mock), MeshVendor::Linkerd).expect("Unable to create");

        assert!(prerouting.add_redirect(69, 420).is_ok());
    }
}
