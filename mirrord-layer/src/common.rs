use std::{
    borrow::Borrow,
    hash::{Hash, Hasher},
    io::SeekFrom,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    path::PathBuf,
};

use mirrord_protocol::{
    CloseFileResponse, OpenFileResponse, OpenOptionsInternal, Port, ReadFileResponse,
    SeekFileResponse, WriteFileResponse,
};
use tokio::sync::oneshot;

#[derive(Debug, Clone)]
pub struct Listen {
    pub fake_port: Port,
    pub real_port: Port,
    pub ipv6: bool,
    pub fd: RawFd,
}

impl PartialEq for Listen {
    fn eq(&self, other: &Self) -> bool {
        self.real_port == other.real_port
    }
}

impl Eq for Listen {}

impl Hash for Listen {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.real_port.hash(state);
    }
}

impl Borrow<Port> for Listen {
    fn borrow(&self) -> &Port {
        &self.real_port
    }
}

impl From<&Listen> for SocketAddr {
    fn from(listen: &Listen) -> Self {
        let address = if listen.ipv6 {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), listen.fake_port)
        } else {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), listen.fake_port)
        };

        debug_assert_eq!(address.port(), listen.fake_port);
        address
    }
}
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
    pub(crate) file_channel_tx: oneshot::Sender<OpenFileResponse>,
    pub(crate) open_options: OpenOptionsInternal,
}

#[derive(Debug)]
pub struct OpenRelativeFileHook {
    pub(crate) relative_fd: RawFd,
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

#[derive(Debug)]
pub struct CloseFileHook {
    pub(crate) fd: RawFd,
    pub(crate) file_channel_tx: oneshot::Sender<CloseFileResponse>,
}

/// These messages are handled internally by -layer, and become `ClientMessage`s sent to -agent.
#[derive(Debug)]
pub enum HookMessage {
    Listen(Listen),
    OpenFileHook(OpenFileHook),
    OpenRelativeFileHook(OpenRelativeFileHook),
    ReadFileHook(ReadFileHook),
    SeekFileHook(SeekFileHook),
    WriteFileHook(WriteFileHook),
    CloseFileHook(CloseFileHook),
}
