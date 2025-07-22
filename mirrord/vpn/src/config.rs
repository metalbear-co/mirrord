use ipnet::IpNet;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::Api;

#[derive(Debug)]
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

        let cluster_config = serde_yaml::from_str::<serde_yaml::Value>(
            kubeadm_configmap.data?.get("ClusterConfiguration")?,
        )
        .inspect_err(|error| tracing::error!(%error, "unable to parse kubeadm config"))
        .ok()?;

        let dns_domain =
            serde_yaml::from_value(cluster_config.get("networking")?.get("dnsDomain")?.clone())
                .ok()?;

        let service_subnet = serde_yaml::from_value::<String>(
            cluster_config
                .get("networking")?
                .get("serviceSubnet")?
                .clone(),
        )
        .ok()?
        .parse()
        .ok()?;

        let kubelet_configmap = api
            .get("kubelet-config")
            .await
            .inspect_err(
                |error| tracing::error!(%error, "unable to fetch kubelet-config configmap"),
            )
            .ok()?;

        let kubelet_config =
            serde_yaml::from_str::<serde_yaml::Value>(kubelet_configmap.data?.get("kubelet")?)
                .inspect_err(|error| tracing::error!(%error, "unable to parse kubeadm config"))
                .ok()?;

        let dns_nameservers =
            serde_yaml::from_value(kubelet_config.get("clusterDNS")?.clone()).ok()?;

        Some(VpnConfig {
            dns_domain,
            dns_nameservers,
            service_subnet,
        })
    }
}
