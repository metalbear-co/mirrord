use ipnet::IpNet;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::Api;

#[derive(Debug, PartialEq, Eq)]
pub struct VpnConfig {
    pub dns_domain: String,
    pub dns_nameservers: Vec<String>,
    pub service_subnet: IpNet,
}

impl VpnConfig {
    pub async fn from_configmaps(api: &Api<ConfigMap>) -> Option<Self> {
        let kubeadm_configmap = api
            .get("kubeadm-config")
            .await
            .inspect_err(
                |error| tracing::error!(%error, "unable to fetch kubeadm-config configmap"),
            )
            .ok()?;

        let (dns_domain, service_subnet) = Self::from_kubeadm_configmap(kubeadm_configmap)?;

        let kubelet_configmap = api
            .get("kubelet-config")
            .await
            .inspect_err(
                |error| tracing::error!(%error, "unable to fetch kubelet-config configmap"),
            )
            .ok()?;

        let dns_nameservers = Self::from_kubelet_configmap(kubelet_configmap)?;

        Some(VpnConfig {
            dns_domain,
            dns_nameservers,
            service_subnet,
        })
    }

    pub fn from_kubeadm_configmap(kubeadm_configmap: ConfigMap) -> Option<(String, IpNet)> {
        let cluster_config = rust_yaml::from_str::<rust_yaml::Value>(
            kubeadm_configmap.data?.get("ClusterConfiguration")?,
        )
        .inspect_err(|error| tracing::error!(%error, "unable to parse kubeadm config"))
        .ok()?;

        let dns_domain = cluster_config
            .get_str("networking")?
            .get_str("dnsDomain")?
            .as_str()?
            .to_owned();

        let service_subnet = cluster_config
            .get_str("networking")?
            .get_str("serviceSubnet")?
            .as_str()?
            .parse()
            .ok()?;

        Some((dns_domain, service_subnet))
    }

    pub fn from_kubelet_configmap(kubelet_configmap: ConfigMap) -> Option<Vec<String>> {
        let kubelet_config =
            rust_yaml::from_str::<rust_yaml::Value>(kubelet_configmap.data?.get("kubelet")?)
                .inspect_err(|error| tracing::error!(%error, "unable to parse kubeadm config"))
                .ok()?;

        let dns_nameservers = kubelet_config
            .get_str("clusterDNS")?
            .as_sequence()?
            .iter()
            .filter_map(|x| Some(x.as_str()?.to_owned()))
            .collect();

        Some(dns_nameservers)
    }
}
