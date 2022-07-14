use mirrord_protocol::{AddrInfoHint, AddrInfoInternal, RemoteResult};
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

pub(crate) type ResponseChannel<T> = oneshot::Sender<RemoteResult<T>>;

#[derive(Debug)]
pub struct GetAddrInfoHook {
    pub(crate) node: Option<String>,
    pub(crate) service: Option<String>,
    pub(crate) hints: Option<AddrInfoHint>,
    pub(crate) hook_channel_tx: ResponseChannel<Vec<AddrInfoInternal>>,
}

/// These messages are handled internally by -layer, and become `ClientMessage`s sent to -agent.
#[derive(Debug)]
pub enum HookMessage {
    Tcp(HookMessageTcp),
    File(HookMessageFile),
    GetAddrInfoHook(GetAddrInfoHook),
}
