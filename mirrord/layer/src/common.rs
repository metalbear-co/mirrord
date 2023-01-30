//! Shared place for a few types and functions that are used everywhere by the layer.
use std::collections::VecDeque;

use mirrord_protocol::{dns::DnsLookup, RemoteResult};
use tokio::sync::oneshot;

use crate::{
    error::{HookError, HookResult},
    file::HookMessageFile,
    outgoing::{tcp::TcpOutgoing, udp::UdpOutgoing},
    tcp::HookMessageTcp,
    HOOK_SENDER,
};

/// Type alias for a queue of responses from the agent, where these responses are [`RemoteResult`]s.
///
/// ## Usage
///
/// We have no identifiers for the hook requests, so if hook responses were sent asynchronously we
/// would have no way to match them back to their requests. However, requests are sent out
/// synchronously, and responses are sent back synchronously, so keeping them in order is how we
/// maintain our way to match them.
///
/// - The usual flow is:
///
/// 1. `push_back` the [`oneshot::Sender`] that will be used to produce the [`RemoteResult`];
///
/// 2. When the operation gets a response from the agent:
///     1. `pop_front` to get the [`oneshot::Sender`], then;
///     2. `Sender::send` the result back to the operation that initiated the request.
pub(crate) type ResponseDeque<T> = VecDeque<ResponseChannel<T>>;

/// Type alias for the channel that sends a response from the agent.
///
/// See [`ResponseDeque`] for usage details.
pub(crate) type ResponseChannel<T> = oneshot::Sender<RemoteResult<T>>;

/// Sends a [`HookMessage`] through the global [`HOOK_SENDER`].
///
/// ## Usage
///
/// - [`file::ops`](crate::file::ops): most of the file operations are blocking, and thus this
///   function is extensively used there;
///
/// - [`socket::ops`](crate::socket::ops): used by some functions that are _blocking-ish_.
pub(crate) fn blocking_send_hook_message(message: HookMessage) -> HookResult<()> {
    HOOK_SENDER
        .get()
        .ok_or(HookError::EmptyHookSender)
        .and_then(|hook_sender| hook_sender.blocking_send(message).map_err(Into::into))
}

/// Hook message for the `socket::getaddrinfo` operation.
///
/// Used to perform a DNS lookup in the agent context.
#[derive(Debug)]
pub struct GetAddrInfoHook {
    /// Host name, or host address.
    pub(crate) node: String,

    /// [`ResponseChannel`] used to send a [`DnsLookup`] response from the agent back to
    /// `socket::getaddrinfo`.
    pub(crate) hook_channel_tx: ResponseChannel<DnsLookup>,
}

/// These messages are handled internally by the layer, and become `ClientMessage`s sent to
/// the agent.
///
/// Most layer operations will send a [`HookMessage`] that will be converted to an equivalent
/// `ClientMessage` after some internal handling is done. Usually this means taking a sender
/// channel from this message, and pushing it into a [`ResponseDeque`], while taking the other
/// fields of the message to become a `ClientMessage`.
#[derive(Debug)]
pub(crate) enum HookMessage {
    /// TCP incoming messages originating from a hook, see [`HookMessageTcp`].
    Tcp(HookMessageTcp),

    /// TCP outgoing messages originating from a hook, see [`TcpOutgoing`].
    TcpOutgoing(TcpOutgoing),

    /// UDP outgoing messages originating from a hook, see [`UdpOutgoing`].
    UdpOutgoing(UdpOutgoing),

    /// File messages originating from a hook, see [`HookMessageFile`].
    File(HookMessageFile),

    /// Message originating from `getaddrinfo`, see [`GetAddrInfoHook`].
    GetAddrInfoHook(GetAddrInfoHook),
}
