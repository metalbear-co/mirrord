use std::{collections::VecDeque, net::IpAddr};

use mirrord_protocol::{AddrInfoHint, AddrInfoInternal, RemoteResult};
use tokio::sync::oneshot;
use trust_dns_resolver::config::Protocol;

use crate::{
    error::{HookError, HookResult},
    file::HookMessageFile,
    outgoing::{tcp::TcpOutgoing, udp::UdpOutgoing},
    tcp::HookMessageTcp,
    HOOK_SENDER,
};

pub(crate) type ResponseDeque<T> = VecDeque<ResponseChannel<T>>;

pub(crate) fn blocking_send_hook_message(message: HookMessage) -> HookResult<()> {
    unsafe {
        HOOK_SENDER
            .as_ref()
            .ok_or(HookError::EmptyHookSender)
            .and_then(|hook_sender| hook_sender.blocking_send(message).map_err(Into::into))
    }
}

pub(crate) type ResponseChannel<T> = oneshot::Sender<RemoteResult<T>>;

#[derive(Debug)]
pub struct GetAddrInfoHook {
    pub(crate) node: Option<String>,
    pub(crate) service: Option<String>,
    pub(crate) protocol: Option<Protocol>,
    pub(crate) hook_channel_tx: ResponseChannel<Vec<IpAddr>>,
}

/// These messages are handled internally by -layer, and become `ClientMessage`s sent to -agent.
#[derive(Debug)]
pub(crate) enum HookMessage {
    Tcp(HookMessageTcp),
    TcpOutgoing(TcpOutgoing),
    UdpOutgoing(UdpOutgoing),
    File(HookMessageFile),
    GetAddrInfoHook(GetAddrInfoHook),
}
