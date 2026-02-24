use std::{net::IpAddr, ops::Deref};

use mirrord_config::feature::network::{
    dns::{DnsConfig, DnsFilterConfig},
    filter::AddressFilter,
};
use tracing::Level;

use crate::{
    detour::Bypass,
    error::{HookError, HookResult},
};

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
    #[tracing::instrument(level = Level::DEBUG, ret)]
    pub fn check_query(&self, node: &str, port: u16) -> HookResult<()> {
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
            Err(HookError::Bypass(Bypass::LocalDns))
        } else {
            Ok(())
        }
    }
}

impl From<&DnsConfig> for DnsSelector {
    fn from(value: &DnsConfig) -> Self {
        if !value.enabled {
            return Self {
                filters: Default::default(),
                filter_is_local: false,
            };
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
                filter
                    .parse::<AddressFilter>()
                    .expect("bad address filter, should be verified in the CLI")
            })
            .collect();

        Self {
            filters,
            filter_is_local,
        }
    }
}
