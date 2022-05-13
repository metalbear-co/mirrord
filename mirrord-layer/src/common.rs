use std::{
    os::unix::{io::RawFd, prelude::*},
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::sync::Notify;

pub type Port = u16;

#[derive(Debug)]
pub struct Listen {
    pub fake_port: Port,
    pub real_port: Port,
    pub ipv6: bool,
    pub fd: RawFd,
}

#[derive(Debug)]
pub struct Open {
    pub(crate) path: PathBuf,
}

// TODO(alex) [high] 2022-05-11: Write down the message flow.
/// These are the messages that `mirrord-layer` sends to ?.
#[derive(Debug)]
pub enum HookMessage {
    Listen(Listen),
    Open(Open),
}
