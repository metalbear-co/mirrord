use std::{os::unix::io::RawFd, path::PathBuf};

use futures::channel::oneshot;
use mirrord_protocol::FileOpenResponse;

pub type Port = u16;

#[derive(Debug)]
pub struct Listen {
    pub fake_port: Port,
    pub real_port: Port,
    pub ipv6: bool,
    pub fd: RawFd,
}

#[derive(Debug)]
pub struct OpenFileHook {
    pub(crate) path: PathBuf,
    pub(crate) file_channel_tx: oneshot::Sender<FileOpenResponse>,
}

/// These messages are handled internally by -layer, and become `ClientMessage`s sent to -agent.
#[derive(Debug)]
pub enum HookMessage {
    Listen(Listen),
    OpenFileHook(OpenFileHook),
}
