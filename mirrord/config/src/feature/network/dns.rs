use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigContext, ConfigError},
    util::{MirrordToggleableConfig, VecOrSingle},
};

#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum DnsFilterConfig {
    /// Traffic that matches what's specified here will go through the remote pod, everything else
    /// will go through local.
    Remote(VecOrSingle<String>),

    /// Traffic that matches what's specified here will go through the local app, everything else
    /// will go through the remote pod.
    Local(VecOrSingle<String>),
}

#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "DnsFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct DnsConfig {
    /// #### feature.network.dns.enabled {#feature-network-dns}
    ///
    /// Resolve DNS via the remote pod.
    ///
    /// Defaults to `true`.
    ///
    /// - Caveats: DNS resolving can be done in multiple ways, some frameworks will use
    /// `getaddrinfo`, while others will create a connection on port `53` and perform a sort
    /// of manual resolution. Just enabling the `dns` feature in mirrord might not be enough.
    /// If you see an address resolution error, try enabling the [`fs`](#feature-fs) feature,
    /// and setting `read_only: ["/etc/resolv.conf"]`.
    #[config(env = "MIRRORD_REMOTE_DNS", default = true)]
    pub enabled: bool,

    /// #### feature.network.dns.filter {#feature-network-dns-filter}
    ///
    /// Unstable: the precise syntax of this config is subject to change.
    #[config(default, unstable)]
    pub filter: Option<DnsFilterConfig>,
}

impl MirrordToggleableConfig for DnsFileConfig {
    fn disabled_config(context: &mut ConfigContext) -> Result<Self::Generated, ConfigError> {
        Ok(DnsConfig {
            enabled: FromEnv::new("MIRRORD_REMOTE_DNS")
                .source_value(context)
                .unwrap_or(Ok(false))?,
            ..Default::default()
        })
    }
}

impl CollectAnalytics for &DnsConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("enabled", self.enabled);

        if let Some(filter) = self.filter.as_ref() {
            match filter {
                DnsFilterConfig::Remote(value) => analytics.add("dns_filter_remote", value.len()),

                DnsFilterConfig::Local(value) => analytics.add("dns_filter_local", value.len()),
            }
        }
    }
}
