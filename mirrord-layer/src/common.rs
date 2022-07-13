use mirrord_protocol::{AddrInfoHint, GetAddrInfoResponse};
use tokio::sync::oneshot;

use crate::{error::LayerError, file::HookMessageFile, tcp::HookMessageTcp, HOOK_SENDER};

pub(crate) fn blocking_send_hook_message(message: HookMessage) -> Result<(), LayerError> {
    unsafe {
        HOOK_SENDER
            .as_ref()
            .ok_or(LayerError::EmptyHookSender)
            .and_then(|hook_sender| hook_sender.blocking_send(message).map_err(Into::into))
    }
}

#[derive(Debug)]
pub struct GetAddrInfoHook {
    pub(crate) node: Option<String>,
    pub(crate) service: Option<String>,
    pub(crate) hints: Option<AddrInfoHint>,
    pub(crate) hook_channel_tx: oneshot::Sender<GetAddrInfoResponse>,
}

/// These messages are handled internally by -layer, and become `ClientMessage`s sent to -agent.
#[derive(Debug)]
pub enum HookMessage {
    Tcp(HookMessageTcp),
    File(HookMessageFile),
    GetAddrInfoHook(GetAddrInfoHook),
}
