use mirrord_protocol::dns::DnsLookup;

use crate::common::ResponseChannel;

/// Hook message for the `socket::getaddrinfo` operation.
///
/// Used to perform a DNS lookup in the agent context.
///
/// - Part of [`HookMessage`](super::common::HookMessage).
#[derive(Debug)]
pub(super) struct GetAddrInfo {
    /// Host name, or host address.
    pub(crate) node: String,

    /// [`ResponseChannel`] used to send a [`DnsLookup`] response from the agent back to
    /// `socket::getaddrinfo`.
    pub(crate) hook_channel_tx: ResponseChannel<DnsLookup>,
}
