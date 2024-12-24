use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use fancy_regex::Regex;
use mirrord_protocol::{MeshVendor, Port};

use crate::{
    error::Result,
    steal::ip_tables::{
        output::OutputRedirect, prerouting::PreroutingRedirect, redirect::Redirect, IPTables,
        IPTABLE_MESH,
    },
};
pub mod istio;

static MULTIPORT_SKIP_PORTS_LOOKUP_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-p tcp -m multiport --dports ([\d:,]+)").unwrap());

static TCP_SKIP_PORTS_LOOKUP_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-p tcp -m tcp --dport ([\d:,]+)").unwrap());

pub(crate) struct MeshRedirect<IPT: IPTables> {
    prerouting: PreroutingRedirect<IPT>,
    output: OutputRedirect<false, IPT>,
    vendor: MeshVendor,
}

impl<IPT> MeshRedirect<IPT>
where
    IPT: IPTables,
{
    pub fn create(ipt: Arc<IPT>, vendor: MeshVendor, pod_ips: Option<&str>) -> Result<Self> {
        let prerouting = PreroutingRedirect::create_prerouting(ipt.clone())?;

        for port in Self::get_skip_ports(&ipt, &vendor)? {
            prerouting.add_rule(&format!("-m multiport -p tcp ! --dports {port} -j RETURN"))?;
        }

        let output = OutputRedirect::create(ipt, IPTABLE_MESH.to_string(), pod_ips)?;

        Ok(MeshRedirect {
            prerouting,
            output,
            vendor,
        })
    }

    pub fn load(ipt: Arc<IPT>, vendor: MeshVendor) -> Result<Self> {
        let prerouting = PreroutingRedirect::load_prerouting(ipt.clone())?;
        let output = OutputRedirect::load(ipt, IPTABLE_MESH.to_string())?;

        Ok(MeshRedirect {
            prerouting,
            output,
            vendor,
        })
    }

    fn get_skip_ports(ipt: &IPT, vendor: &MeshVendor) -> Result<Vec<String>> {
        let chain_name = vendor.input_chain();
        let lookup_regex = if let Some(regex) = vendor.skip_ports_regex() {
            regex
        } else {
            return Ok(vec![]);
        };

        let skipped_ports = ipt
            .list_rules(chain_name)?
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

#[async_trait]
impl<IPT> Redirect for MeshRedirect<IPT>
where
    IPT: IPTables + Send + Sync,
{
    async fn mount_entrypoint(&self) -> Result<()> {
        self.prerouting.mount_entrypoint().await?;
        self.output.mount_entrypoint().await?;

        Ok(())
    }

    async fn unmount_entrypoint(&self) -> Result<()> {
        self.prerouting.unmount_entrypoint().await?;
        self.output.unmount_entrypoint().await?;

        Ok(())
    }

    async fn add_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        if self.vendor != MeshVendor::IstioCni {
            self.prerouting
                .add_redirect(redirected_port, target_port)
                .await?;
        }
        self.output
            .add_redirect(redirected_port, target_port)
            .await?;

        Ok(())
    }

    async fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()> {
        if self.vendor != MeshVendor::IstioCni {
            self.prerouting
                .remove_redirect(redirected_port, target_port)
                .await?;
        }
        self.output
            .remove_redirect(redirected_port, target_port)
            .await?;

        Ok(())
    }
}

/// Extends the [`MeshVendor`] type with methods that are only relevant for the agent.
pub(super) trait MeshVendorExt: Sized {
    fn detect<IPT: IPTables>(ipt: &IPT) -> Result<Option<Self>>;
    fn input_chain(&self) -> &str;
    fn skip_ports_regex(&self) -> Option<&Regex>;
}

impl MeshVendorExt for MeshVendor {
    fn detect<IPT: IPTables>(ipt: &IPT) -> Result<Option<Self>> {
        if let Ok(val) = std::env::var("MIRRORD_AGENT_ISTIO_CNI")
            && val.to_lowercase() == "true"
        {
            return Ok(Some(MeshVendor::IstioCni));
        }
        let output = ipt.list_rules("OUTPUT")?;

        let nat_result = output.iter().find_map(|rule| {
            if rule.contains("-j PROXY_INIT_OUTPUT") {
                Some(MeshVendor::Linkerd)
            } else if rule.contains("-j ISTIO_OUTPUT") {
                Some(MeshVendor::Istio)
            } else if rule.contains("-j KUMA_MESH_OUTBOUND") {
                Some(MeshVendor::Kuma)
            } else {
                None
            }
        });

        match &nat_result {
            Some(MeshVendor::Istio) => {
                let is_ambient = ipt
                    .with_table("mangle")
                    .list_rules("OUTPUT")?
                    .iter()
                    .any(|rule| rule.contains("-j ISTIO_OUTPUT"));

                Ok(Some(if is_ambient {
                    MeshVendor::IstioAmbient
                } else {
                    MeshVendor::Istio
                }))
            }
            _ => Ok(nat_result),
        }
    }

    fn input_chain(&self) -> &str {
        match self {
            MeshVendor::Linkerd => "PROXY_INIT_REDIRECT",
            MeshVendor::Istio | MeshVendor::IstioAmbient | MeshVendor::IstioCni => "ISTIO_INBOUND",
            MeshVendor::Kuma => "KUMA_MESH_INBOUND",
        }
    }

    fn skip_ports_regex(&self) -> Option<&Regex> {
        match self {
            MeshVendor::Linkerd => Some(&MULTIPORT_SKIP_PORTS_LOOKUP_REGEX),
            MeshVendor::Istio | MeshVendor::IstioAmbient => Some(&TCP_SKIP_PORTS_LOOKUP_REGEX),
            MeshVendor::Kuma => Some(&TCP_SKIP_PORTS_LOOKUP_REGEX),
            MeshVendor::IstioCni => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use mockall::predicate::*;
    use nix::unistd::getgid;

    use super::*;
    use crate::steal::ip_tables::{MockIPTables, IPTABLE_PREROUTING};

    fn create_mesh_list_values(mock: &mut MockIPTables) {
        mock.expect_list_rules()
            .with(eq("OUTPUT"))
            .returning(|_| Ok(vec!["-j PROXY_INIT_OUTPUT".to_owned()]));

        mock.expect_list_rules()
            .with(eq("PROXY_INIT_REDIRECT"))
            .returning(|_| {
                Ok(vec![
                    "-N PROXY_INIT_REDIRECT".to_owned(),
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
    }

    #[tokio::test]
    async fn add_redirect() {
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
                eq(format!("-m owner --gid-owner {gid} -p tcp  -j RETURN")),
                eq(1),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_insert_rule()
            .with(
                eq(IPTABLE_MESH.as_str()),
                eq("-o lo -m tcp -p tcp --dport 69 -j REDIRECT --to-ports 420"),
                eq(2),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_PREROUTING.as_str()))
            .times(1)
            .returning(|_| Ok(()));

        mock.expect_remove_chain()
            .with(eq(IPTABLE_MESH.as_str()))
            .times(1)
            .returning(|_| Ok(()));

        let prerouting = MeshRedirect::create(Arc::new(mock), MeshVendor::Linkerd, None)
            .expect("Unable to create");

        assert!(prerouting.add_redirect(69, 420).await.is_ok());
    }
}
