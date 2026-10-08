use std::{net::IpAddr, ops::Deref};

use mirrord_config::feature::network::{
    dns::{DnsConfig, DnsFilterConfig},
    filter::{AddressFilter, AddressFilterError},
};

use crate::{
    detour::{Bypass, Detour},
    setup::SetupError,
};

/// A `feature.network.dns.filter` entry that does not parse.
#[derive(Debug, thiserror::Error)]
#[error("{filter:?}: {source}")]
pub struct DnsFilterError {
    pub filter: String,
    #[source]
    pub source: AddressFilterError,
}

/// Generated from [`DnsConfig`] provided in the [`LayerConfig`](mirrord_config::LayerConfig).
/// Decides whether DNS queries are done locally or remotely.
#[derive(Debug)]
pub struct DnsSelector {
    /// Filters provided in the config.
    filters: Vec<AddressFilter>,
    /// Whether a query matching one of [`Self::filters`] should be done locally.
    filter_is_local: bool,
}

impl DnsSelector {
    /// Bypasses queries that should be done locally.
    #[mirrord_layer_macro::instrument(level = tracing::Level::DEBUG, ret)]
    pub fn check_query(&self, node: &str, port: u16) -> Detour<()> {
        let matched = self
            .filters
            .iter()
            .filter(|filter| {
                let filter_port = filter.port();
                filter_port == 0 || filter_port == port
            })
            .any(|filter| match filter {
                AddressFilter::Port(..) => true,
                AddressFilter::Name(filter_name, _) => filter_name == node,
                AddressFilter::Socket(filter_socket) => {
                    filter_socket.ip().is_unspecified()
                        || Some(filter_socket.ip()) == node.parse().ok()
                }
                AddressFilter::Subnet(filter_subnet, _) => {
                    let Ok(ip) = node.parse::<IpAddr>() else {
                        return false;
                    };

                    filter_subnet.contains(&ip)
                }
            });

        if matched == self.filter_is_local {
            Detour::Bypass(Bypass::LocalDns)
        } else {
            Detour::Success(())
        }
    }
}

/// Fails with [`SetupError::DnsFilter`] on a filter that does not parse.
impl TryFrom<&DnsConfig> for DnsSelector {
    type Error = SetupError;

    fn try_from(value: &DnsConfig) -> Result<Self, Self::Error> {
        if !value.enabled {
            return Ok(Self {
                filters: Default::default(),
                filter_is_local: false,
            });
        }

        let (filters, filter_is_local) = match &value.filter {
            Some(DnsFilterConfig::Local(filters)) => (Some(filters.deref()), true),
            Some(DnsFilterConfig::Remote(filters)) => (Some(filters.deref()), false),
            None => (None, true),
        };

        let filters = filters
            .into_iter()
            .flatten()
            .map(|filter| {
                filter.parse().map_err(|source| DnsFilterError {
                    filter: filter.to_owned(),
                    source,
                })
            })
            .collect::<Result<_, DnsFilterError>>()?;

        Ok(Self {
            filters,
            filter_is_local,
        })
    }
}

#[cfg(test)]
mod tests {
    use mirrord_config::util::VecOrSingle;

    use super::*;

    /// The setup error names the filter that does not parse.
    #[test]
    fn an_invalid_filter_is_named_in_the_setup_error() {
        let filter = "google.com/24:7777";
        let config = DnsConfig {
            enabled: true,
            filter: Some(DnsFilterConfig::Remote(VecOrSingle::Single(
                filter.to_owned(),
            ))),
        };

        let error =
            DnsSelector::try_from(&config).expect_err("a hostname with a subnet does not parse");

        assert!(
            matches!(
                &error,
                SetupError::DnsFilter(DnsFilterError { filter: named, .. }) if named == filter
            ),
            "{error:?}"
        );
    }
}
