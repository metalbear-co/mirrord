//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    os::unix::io::RawFd,
    sync::{Arc, LazyLock, Mutex},
};

use libc::{c_int, sockaddr, socklen_t};
use mirrord_protocol::{AddrInfoHint, Port};
use socket2::SockAddr;

use crate::error::{HookError, HookResult as Result};

pub(super) mod hooks;
mod ops;

pub(crate) static SOCKETS: LazyLock<Mutex<HashMap<RawFd, Arc<MirrorSocket>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub static CONNECTION_QUEUE: LazyLock<Mutex<ConnectionQueue>> =
    LazyLock::new(|| Mutex::new(ConnectionQueue::default()));

/// Struct sent over the socket once created to pass metadata to the hook
#[derive(Debug)]
pub struct SocketInformation {
    pub address: SocketAddr,
}

/// poll_agent loop inserts connection data into this queue, and accept reads it.
#[derive(Debug, Default)]
pub struct ConnectionQueue {
    connections: HashMap<RawFd, VecDeque<SocketInformation>>,
}

impl ConnectionQueue {
    pub fn add(&mut self, fd: &RawFd, info: SocketInformation) {
        self.connections.entry(*fd).or_default().push_back(info);
    }
    pub fn get(&mut self, fd: &RawFd) -> Option<SocketInformation> {
        let mut queue = self.connections.remove(fd)?;
        if let Some(info) = queue.pop_front() {
            if !queue.is_empty() {
                self.connections.insert(*fd, queue);
            }
            Some(info)
        } else {
            None
        }
    }
}

impl SocketInformation {
    pub fn new(address: SocketAddr) -> Self {
        Self { address }
    }
}

trait GetPeerName {
    fn get_peer_name(&self) -> SocketAddr;
}

#[derive(Debug)]
pub struct Connected {
    /// Remote address we're connected to
    remote_address: SocketAddr,
    /// Local address it's connected from
    local_address: SocketAddr,
}

#[derive(Debug, Clone, Copy)]
pub struct Bound {
    address: SocketAddr,
}

#[derive(Debug)]
pub enum SocketState {
    Initialized,
    Bound(Bound),
    Listening(Bound),
    Connected(Connected),
}

impl Default for SocketState {
    fn default() -> Self {
        SocketState::Initialized
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct MirrorSocket {
    domain: c_int,
    type_: c_int,
    protocol: c_int,
    pub state: SocketState,
}

#[inline]
const fn is_ignored_port(port: Port) -> bool {
    port == 0 || (port > 50000 && port < 60000)
}

/// Fill in the sockaddr structure for the given address.
#[inline]
fn fill_address(
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_address: SocketAddr,
) -> Result<()> {
    if address.is_null() {
        Ok(())
    } else if address_len.is_null() {
        Err(HookError::NullPointer)
    } else {
        let os_address = SockAddr::from(new_address);

        unsafe {
            let len = std::cmp::min(*address_len as usize, os_address.len() as usize);

            std::ptr::copy_nonoverlapping(
                os_address.as_ptr() as *const u8,
                address as *mut u8,
                len,
            );

            *address_len = os_address.len();
        }

        Ok(())
    }
}

pub(crate) trait AddrInfoHintExt {
    fn from_raw(raw: libc::addrinfo) -> Self;
}

impl AddrInfoHintExt for AddrInfoHint {
    fn from_raw(raw: libc::addrinfo) -> Self {
        Self {
            ai_family: raw.ai_family,
            ai_socktype: raw.ai_socktype,
            ai_protocol: raw.ai_protocol,
            ai_flags: raw.ai_flags,
        }
    }
}
