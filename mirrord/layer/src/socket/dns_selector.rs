use std::{net::IpAddr, ops::Deref};

use mirrord_config::feature::network::{
    dns::{DnsConfig, DnsFilterConfig},
    filter::AddressFilter,
};

use crate::detour::{Bypass, Detour};

#[derive(Debug)]
pub struct DnsSelector {
    filters: Vec<AddressFilter>,
    filter_is_local: bool,
}

impl DnsSelector {
    pub fn check_query(&self, node: &str, port: u16) -> Detour<()> {
        let matched = self
            .filters
            .iter()
            .filter(|filter| {
                let filter_port = filter.port();
                filter_port == 0 || filter_port == port
            })
            .any(|filter| match filter {
                AddressFilter::Name(filter_name, _) => filter_name == node,
                AddressFilter::Socket(filter_socket) => {
                    Some(filter_socket.ip()) == node.parse().ok()
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
            None => (None, false),
        };

        let filters = filters
            .into_iter()
            .flatten()
            .map(|filter| filter.parse::<AddressFilter>().expect("bad address filter"))
            .collect();

        Self {
            filters,
            filter_is_local,
        }
    }
}
