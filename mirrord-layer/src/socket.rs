//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    sync::{Arc, LazyLock, Mutex},
};

use errno::{set_errno, Errno};
use libc::{c_int, sockaddr, socklen_t};
use mirrord_protocol::Port;
use os_socketaddr::OsSocketAddr;
use tracing::warn;

use crate::error::LayerError;

pub(crate) mod hooks;
pub(crate) mod ops;

pub(crate) static SOCKETS: LazyLock<Mutex<HashMap<RawFd, Arc<Socket>>>> =
    LazyLock::new(|| Mutex::new(HashMap::default()));

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connected {
    /// Remote address we're connected to
    remote_address: SocketAddr,
    /// Local address it's connected from
    local_address: SocketAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bound {
    address: SocketAddr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
pub struct Socket {
    domain: c_int,
    type_: c_int,
    protocol: c_int,
    pub state: SocketState,
}

impl TryFrom<&Socket> for OsSocketAddr {
    type Error = LayerError;

    fn try_from(socket: &Socket) -> Result<Self, Self::Error> {
        match socket.domain {
            libc::AF_INET => Ok(OsSocketAddr::from(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                0,
            ))),
            libc::AF_INET6 => Ok(OsSocketAddr::from(SocketAddr::new(
                IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                0,
            ))),
            invalid_domain => {
                // shouldn't happen
                warn!("unsupported domain");
                Err(LayerError::UnsupportedDomain(invalid_domain))
            }
        }
    }
}

#[inline]
const fn is_ignored_port(port: Port) -> bool {
    port == 0 || (port > 50000 && port < 60000)
}

/// Fill in the sockaddr structure for the given address.
#[inline]
pub(crate) fn fill_address(
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_address: SocketAddr,
) -> c_int {
    if address.is_null() {
        return 0;
    }

    if address_len.is_null() {
        set_errno(Errno(libc::EINVAL));
        return -1;
    }
    let os_address: OsSocketAddr = new_address.into();
    unsafe {
        let len = std::cmp::min(*address_len as usize, os_address.len() as usize);
        std::ptr::copy_nonoverlapping(os_address.as_ptr() as *const u8, address as *mut u8, len);
        *address_len = os_address.len();
    }
    0
}
