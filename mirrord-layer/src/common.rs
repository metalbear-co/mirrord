use std::os::unix::io::RawFd;

pub type Port = u16;

#[derive(Debug)]
pub struct Listen {
    pub fake_port: Port,
    pub real_port: Port,
    pub ipv6: bool,
    pub fd: RawFd,
}

#[derive(Debug)]
pub enum HookMessage {
    Listen(Listen),
}
