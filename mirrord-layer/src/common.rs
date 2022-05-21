use std::{io::SeekFrom, os::unix::io::RawFd, path::PathBuf};

use mirrord_protocol::{OpenFileResponse, ReadFileResponse, SeekFileResponse};
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
    pub(crate) buffer_size: usize,
    pub(crate) file_channel_tx: oneshot::Sender<ReadFileResponse>,
}

#[derive(Debug)]
pub struct SeekFileHook {
    pub(crate) fd: RawFd,
    pub(crate) seek_from: SeekFrom,
    pub(crate) file_channel_tx: oneshot::Sender<SeekFileResponse>,
}

/// These messages are handled internally by -layer, and become `ClientMessage`s sent to -agent.
#[derive(Debug)]
pub enum HookMessage {
    Listen(Listen),
    OpenFileHook(OpenFileHook),
    ReadFileHook(ReadFileHook),
    SeekFileHook(SeekFileHook),
}

#[derive(Error, Debug)]
pub enum LayerError {
    #[error("Sender failed with `{0}`!")]
    SendError(#[from] SendError<HookMessage>),

    #[error("Receiver failed with `{0}`!")]
    RecvError(#[from] RecvError),
}
