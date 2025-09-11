//! Unix-specific extensions for OutgoingSelector
//!
//! Provides convenience functions for the OutgoingSelector when running on Unix platforms.

use mirrord_intproxy_protocol::NetProtocol;
use mirrord_layer_lib::socket::{ConnectionThrough, OutgoingSelector};

use crate::{error::HookResult, socket::UnixDnsResolver};

/// Get connection through using the Unix DNS resolver
///
/// This is a convenience function that uses the Unix DNS resolver.
pub fn get_connection_through(
    selector: &OutgoingSelector,
    address: std::net::SocketAddr,
    protocol: NetProtocol,
) -> HookResult<ConnectionThrough> {
    let resolver = UnixDnsResolver;
    selector.get_connection_through_with_resolver(address, protocol, &resolver)
}
