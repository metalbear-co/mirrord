pub mod reverse_dns;
#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod windows;

use std::net::IpAddr;

use libc::c_int;
use mirrord_protocol::dns::{AddressFamily, GetAddrInfoRequestV2, LookupRecord, SockType};
use tracing::Level;

use crate::{
    HookResult,
    proxy_connection::make_proxy_request_with_response,
    socket::{
        AF_INET, AF_INET6, SOCK_DGRAM, SOCK_STREAM,
        dns::reverse_dns::update_dns_reverse_mapping_bulk,
    },
};

/// Handles the remote communication part of [`libc::getaddrinfo`], call this if you want to resolve
/// a DNS query through the agent, but don't need to deal with all the [`libc::getaddrinfo`] stuff.
///
/// # Note
///
/// This function updates the mapping in [`reverse_dns::REMOTE_DNS_REVERSE_MAPPING`].
#[mirrord_layer_macro::instrument(level = Level::TRACE, ret, err)]
pub fn remote_getaddrinfo(
    node: String,
    service_port: u16,
    flags: c_int,
    family: c_int,
    socktype: c_int,
    protocol: c_int,
) -> HookResult<Vec<(String, IpAddr)>> {
    let family = match family {
        AF_INET => AddressFamily::Ipv4Only,
        AF_INET6 => AddressFamily::Ipv6Only,
        _ => AddressFamily::Both,
    };
    let socktype = match socktype {
        SOCK_STREAM => SockType::Stream,
        SOCK_DGRAM => SockType::Dgram,
        _ => SockType::Any,
    };
    let addr_info_list = make_proxy_request_with_response(GetAddrInfoRequestV2 {
        node,
        service_port,
        flags,
        family,
        socktype,
        protocol,
    })?
    .0?;

    let result = addr_info_list
        .into_iter()
        .map(|LookupRecord { name, ip }| (name, ip))
        .collect();

    update_dns_reverse_mapping_bulk(&result);

    Ok(result)
}

/// Perform remote DNS resolution via ProxyConnection using mirrord protocol
/// wrapper around `remote_getaddrinfo` with common parameters
pub fn remote_dns_resolve_via_proxy(hostname: &str) -> HookResult<Vec<(String, std::net::IpAddr)>> {
    remote_getaddrinfo(hostname.to_string(), 0, 0, 0, 0, 0)
}
