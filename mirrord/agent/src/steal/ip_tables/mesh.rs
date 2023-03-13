use std::sync::LazyLock;

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

pub static IPTABLE_MESH_ENV: &str = "MIRRORD_IPTABLE_MESH_NAME";
pub static IPTABLE_MESH: LazyLock<String> = LazyLock::new(|| {
    std::env::var(IPTABLE_MESH_ENV).unwrap_or_else(|_| {
        format!(
            "MIRRORD_MESH_{}",
            Alphanumeric.sample_string(&mut rand::thread_rng(), 5)
        )
    })
});

pub struct MeshRedirect<'ipt, IPT> {
    preroute: PreroutingRedirect<'ipt, IPT>,
    managed: IPTableChain<'ipt, IPT>,
    own_packet_filter: String,
}

impl<'ipt, IPT> MeshRedirect<'ipt, IPT>
where
    IPT: IPTables,
{
    pub fn create(ipt: &'ipt IPT) -> Result<Self> {
        let preroute = PreroutingRedirect::create(ipt)?;
        let managed = IPTableChain::create(ipt, &IPTABLE_MESH)?;

        let gid = getgid();
        managed.add_rule(&format!("-m owner --gid-owner {gid} -p tcp -j RETURN"))?;

        let own_packet_filter = Self::get_own_packet_filter(ipt, "PROXY_INIT_OUTPUT")?;

        Ok(MeshRedirect {
            preroute,
            managed,
            own_packet_filter,
        })
    }

    fn get_own_packet_filter(ipt: &'ipt IPT, chain_name: &str) -> Result<String> {
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
}

impl<IPT> Redirect for MeshRedirect<'_, IPT>
where
    IPT: IPTables,
{
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

        let prerouting = MeshRedirect::create(&mock).expect("Unable to create");

        assert!(prerouting.add_redirect(69, 420).is_ok());
    }
}
