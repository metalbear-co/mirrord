use std::{io::SeekFrom, os::unix::io::RawFd, path::PathBuf};

use mirrord_protocol::{
    OpenFileResponse, OpenOptionsInternal, ReadFileResponse, SeekFileResponse, WriteFileResponse,
};
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

// TODO(alex) [low] 2022-05-22: Alright, all these are pretty much the same thing, they could be
// abstract over a generic dependent-ish type like so:
/*
struct FakeFile<Operation: FileOperation> {
    pub(crate) file_channel_tx: oneshot::Sender<Operation::Response>,
}

struct OpenHook {
    pub(crate) path: PathBuf,
}

impl FileOperation for OpenHook {
    type Response = OpenFileResponse;
}
 */
// But maybe `FakeFile` could be even simpler? These are just some initial thoughts.
//
// An issue I can see right now is that we would be tying file operations to the fake file holder,
// which isn't nice (it would be just the same handling as it is right now, but with improved
// guarantees).
#[derive(Debug)]
pub struct OpenFileHook {
    pub(crate) path: PathBuf,
    pub(crate) file_channel_tx: oneshot::Sender<OpenFileResponse>,
    pub(crate) open_options: OpenOptionsInternal,
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

#[derive(Debug)]
pub struct WriteFileHook {
    pub(crate) fd: RawFd,
    pub(crate) write_bytes: Vec<u8>,
    pub(crate) file_channel_tx: oneshot::Sender<WriteFileResponse>,
}

/// These messages are handled internally by -layer, and become `ClientMessage`s sent to -agent.
#[derive(Debug)]
pub enum HookMessage {
    Listen(Listen),
    OpenFileHook(OpenFileHook),
    ReadFileHook(ReadFileHook),
    SeekFileHook(SeekFileHook),
    WriteFileHook(WriteFileHook),
}

#[derive(Error, Debug)]
pub enum LayerError {
    #[error("Sender failed with `{0}`!")]
    SendError(#[from] SendError<HookMessage>),

    #[error("Receiver failed with `{0}`!")]
    RecvError(#[from] RecvError),
}
