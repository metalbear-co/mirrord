//! Unix-specific extensions for OutgoingSelector
//!
//! Provides convenience methods for the OutgoingSelector when running on Unix platforms.

use mirrord_intproxy_protocol::NetProtocol;
use mirrord_layer_lib::socket::{ConnectionThrough, OutgoingSelector};
use mirrord_protocol::ResponseError;

use crate::{setup, socket::DnsResolver};

/// Unix-specific extensions for OutgoingSelector
impl OutgoingSelector {
    /// Get connection through using the global DNS resolver
    ///
    /// This is a convenience method that uses the global DNS resolver from the setup.
    pub fn get_connection_through(
        &self,
        address: std::net::SocketAddr,
        protocol: NetProtocol,
    ) -> Result<ConnectionThrough, ResponseError> {
        let resolver = setup().dns_resolver();
        self.get_connection_through_with_resolver(address, protocol, resolver)
            .map_err(|_| ResponseError::DnsLookup)
    }
}
