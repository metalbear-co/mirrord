use std::ops::Deref;

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::Deserialize;

use super::filter::AddressFilter;
use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigContext, ConfigError},
    util::{MirrordToggleableConfig, VecOrSingle},
};

/// List of addresses/ports/subnets that should be resolved through either the remote pod or local
/// app, depending how you set this up with either `remote` or `local`.
///
/// You may use this option to specify when DNS resolution is done from the remote pod (which
/// is the default behavior when you enable remote DNS), or from the local app (default when
/// you have remote DNS disabled).
///
/// Takes a list of values, such as:
///
/// - Only queries for hostname `my-service-in-cluster` will go through the remote pod.
///
/// ```json
/// {
///   "remote": ["my-service-in-cluster"]
/// }
/// ```
///
/// - Only queries for addresses in subnet `1.1.1.0/24` with service port `1337`` will go through
///   the remote pod.
///
/// ```json
/// {
///   "remote": ["1.1.1.0/24:1337"]
/// }
/// ```
///
/// - Only queries for hostname `google.com` with service port `1337` or `7331`
/// will go through the remote pod.
///
/// ```json
/// {
///   "remote": ["google.com:1337", "google.com:7331"]
/// }
/// ```
///
/// - Only queries for `localhost` with service port `1337` will go through the local app.
///
/// ```json
/// {
///   "local": ["localhost:1337"]
/// }
/// ```
///
/// - Only queries with service port `1337` or `7331` will go through the local app.
///
/// ```json
/// {
///   "local": [":1337", ":7331"]
/// }
/// ```
///
/// Valid values follow this pattern: `[name|address|subnet/mask][:port]`.
#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum DnsFilterConfig {
    /// DNS queries matching what is specified here will go through the remote pod, everything else
    /// will go through local.
    Remote(VecOrSingle<String>),

    /// DNS queries matching what is specified here will go through the local app, everything else
    /// will go through the remote pod.
    Local(VecOrSingle<String>),
}

/// Resolve DNS via the remote pod.
///
/// Defaults to `true`.
///
/// - Caveats: DNS resolving can be done in multiple ways, some frameworks will use
/// `getaddrinfo`, while others will create a connection on port `53` and perform a sort
/// of manual resolution. Just enabling the `dns` feature in mirrord might not be enough.
/// If you see an address resolution error, try enabling the [`fs`](#feature-fs) feature,
/// and setting `read_only: ["/etc/resolv.conf"]`.
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "DnsFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct DnsConfig {
    #[config(env = "MIRRORD_REMOTE_DNS", default = true)]
    pub enabled: bool,

    /// #### feature.network.dns.filter {#feature-network-dns-filter}
    ///
    /// Unstable: the precise syntax of this config is subject to change.
    #[config(default, unstable)]
    pub filter: Option<DnsFilterConfig>,
}

impl DnsConfig {
    pub fn verify(&self, context: &mut ConfigContext) -> Result<(), ConfigError> {
        let filters = match &self.filter {
            Some(..) if !self.enabled => {
                context.add_warning(
                    "Remote DNS resolution is disabled, provided DNS filter will be ignored"
                        .to_string(),
                );
                return Ok(());
            }
            None => return Ok(()),
            Some(DnsFilterConfig::Local(filters)) if filters.is_empty() => {
                context.add_warning(
                    "Local DNS filter is empty, all DNS resolution will be done remotely"
                        .to_string(),
                );
                return Ok(());
            }
            Some(DnsFilterConfig::Remote(filters)) if filters.is_empty() => {
                context.add_warning(
                    "Remote DNS filter is empty, all DNS resolution will be done locally"
                        .to_string(),
                );
                return Ok(());
            }
            Some(DnsFilterConfig::Local(filters)) => filters.deref(),
            Some(DnsFilterConfig::Remote(filters)) => filters.deref(),
        };

        for filter in filters {
            let Err(error) = filter.parse::<AddressFilter>() else {
                continue;
            };

            return Err(ConfigError::InvalidValue(
                filter.to_string(),
                "DNS filter",
                error.to_string(),
            ));
        }

        Ok(())
    }
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
