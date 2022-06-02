use std::{
    borrow::Borrow,
    hash::{Hash, Hasher},
    os::unix::io::RawFd,
};

use mirrord_protocol::Port;

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
#[derive(Debug)]
pub enum HookMessage {
    Listen(Listen),
}
