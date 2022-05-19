use std::{os::unix::io::RawFd, path::PathBuf};

use mirrord_protocol::{OpenFileResponse, ReadFileResponse};
use thiserror::Error;
use tokio::sync::{
    mpsc::error::SendError,
    oneshot::{self, error::RecvError},
};

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
    pub(crate) file_channel_tx: oneshot::Sender<OpenFileResponse>,
}

#[derive(Debug)]
pub struct ReadFileHook {
    pub(crate) fd: RawFd,
    pub(crate) file_channel_tx: oneshot::Sender<ReadFileResponse>,
    pub(crate) buffer_size: usize,
}

/// These messages are handled internally by -layer, and become `ClientMessage`s sent to -agent.
#[derive(Debug)]
pub enum HookMessage {
    Listen(Listen),
    OpenFileHook(OpenFileHook),
    ReadFileHook(ReadFileHook),
}

#[derive(Error, Debug)]
pub enum LayerError {
    #[error("Sender failed with `{0}`!")]
    SendError(#[from] SendError<HookMessage>),

    #[error("Receiver failed with `{0}`!")]
    RecvError(#[from] RecvError),
}
