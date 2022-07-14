use std::{io::SeekFrom, path::PathBuf};

use mirrord_protocol::{
    AddrInfoHint, AddrInfoInternal, CloseFileResponse, OpenFileResponse, OpenOptionsInternal,
    ReadFileResponse, RemoteResult, SeekFileResponse, WriteFileResponse,
};
use tokio::sync::oneshot;

use crate::{error::LayerError, tcp::HookMessageTcp, HOOK_SENDER};

pub(crate) fn blocking_send_hook_message(message: HookMessage) -> Result<(), LayerError> {
    unsafe {
        HOOK_SENDER
            .as_ref()
            .ok_or(LayerError::EmptyHookSender)
            .and_then(|hook_sender| hook_sender.blocking_send(message).map_err(Into::into))
    }
}

pub(crate) type ResponseSender<T> = oneshot::Sender<RemoteResult<T>>;

// TODO: Some ideas around abstracting file operations:
// Alright, all these are pretty much the same thing, they could be
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
    pub(crate) file_channel_tx: ResponseSender<OpenFileResponse>,
    pub(crate) open_options: OpenOptionsInternal,
}

#[derive(Debug)]
pub struct OpenRelativeFileHook {
    pub(crate) relative_fd: usize,
    pub(crate) path: PathBuf,
    pub(crate) file_channel_tx: ResponseSender<OpenFileResponse>,
    pub(crate) open_options: OpenOptionsInternal,
}

#[derive(Debug)]
pub struct ReadFileHook {
    pub(crate) fd: usize,
    pub(crate) buffer_size: usize,
    pub(crate) file_channel_tx: ResponseSender<ReadFileResponse>,
}

#[derive(Debug)]
pub struct SeekFileHook {
    pub(crate) fd: usize,
    pub(crate) seek_from: SeekFrom,
    pub(crate) file_channel_tx: ResponseSender<SeekFileResponse>,
}

#[derive(Debug)]
pub struct WriteFileHook {
    pub(crate) fd: usize,
    pub(crate) write_bytes: Vec<u8>,
    pub(crate) file_channel_tx: ResponseSender<WriteFileResponse>,
}

#[derive(Debug)]
pub struct CloseFileHook {
    pub(crate) fd: usize,
    pub(crate) file_channel_tx: ResponseSender<CloseFileResponse>,
}

#[derive(Debug)]
pub struct GetAddrInfoHook {
    pub(crate) node: Option<String>,
    pub(crate) service: Option<String>,
    pub(crate) hints: Option<AddrInfoHint>,
    pub(crate) hook_channel_tx: ResponseSender<Vec<AddrInfoInternal>>,
}

/// These messages are handled internally by -layer, and become `ClientMessage`s sent to -agent.
#[derive(Debug)]
pub enum HookMessage {
    Tcp(HookMessageTcp),
    OpenFileHook(OpenFileHook),
    OpenRelativeFileHook(OpenRelativeFileHook),
    ReadFileHook(ReadFileHook),
    SeekFileHook(SeekFileHook),
    WriteFileHook(WriteFileHook),
    CloseFileHook(CloseFileHook),
    GetAddrInfoHook(GetAddrInfoHook),
}
