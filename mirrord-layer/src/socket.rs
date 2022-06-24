//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    sync::{Arc, LazyLock, Mutex},
};

use libc::{c_int, sockaddr, socklen_t};
use mirrord_protocol::Port;
use os_socketaddr::OsSocketAddr;
use socket2::Socket;
use tracing::warn;

use crate::error::LayerError;

pub(crate) mod hooks;
pub(crate) mod ops;

pub(crate) static MIRROR_SOCKETS: LazyLock<Mutex<HashMap<RawFd, MirrorSocket>>> =
    LazyLock::new(|| Mutex::new(HashMap::default()));

pub(crate) static BYPASS_SOCKETS: LazyLock<Mutex<HashMap<RawFd, Socket>>> =
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
    mirror_address: SocketAddr,
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
pub struct MirrorSocket {
    inner: Socket,
    pub state: SocketState,
}

impl MirrorSocket {
    fn get_bound(&self) -> Result<Bound, LayerError> {
        if let SocketState::Bound(bound) = &self.state {
            Ok(*bound)
        } else {
            Err(LayerError::SocketInvalidState)
        }
    }

    fn get_connected_remote_address(&self) -> Result<SocketAddr, LayerError> {
        if let SocketState::Connected(connected) = &self.state {
            Ok(connected.remote_address)
        } else {
            Err(LayerError::SocketInvalidState)
        }
    }

    fn get_local_address(&self) -> Result<SocketAddr, LayerError> {
        match &self.state {
            SocketState::Initialized => Err(LayerError::SocketInvalidState),
            SocketState::Bound(bound) => Ok(bound.address),
            SocketState::Listening(listening) => Ok(listening.address),
            SocketState::Connected(connected) => Ok(connected.local_address),
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
) -> Result<(), LayerError> {
    if address.is_null() {
        Err(LayerError::NullSocketAddress)
    } else if address_len.is_null() {
        Err(LayerError::NullAddressLength)
    } else {
        let os_address: OsSocketAddr = new_address.into();
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
