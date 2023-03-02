//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    sync::{atomic::AtomicU64, Arc, LazyLock, Mutex},
};

use libc::{c_int, sockaddr, socklen_t};
use mirrord_protocol::Port;
use socket2::SockAddr;
use tracing::warn;
use trust_dns_resolver::config::Protocol;

use crate::{
    detour::{Bypass, Detour, OptionExt},
    error::{HookError, HookResult},
};

pub(super) mod hooks;
pub(crate) mod ops;

pub(crate) static SOCKET_ALLOCATOR: AtomicU64 = AtomicU64::new(0);

pub(crate) static SOCKETS: LazyLock<Mutex<HashMap<RawFd, Arc<UserSocket>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub static CONNECTION_QUEUE: LazyLock<Mutex<ConnectionQueue>> =
    LazyLock::new(|| Mutex::new(ConnectionQueue::default()));

#[derive(Debug, PartialOrd, PartialEq, Ord, Eq, Clone, Copy, Hash)]
pub(crate) struct SocketId(u64);

impl SocketId {
    pub(crate) fn new() -> Self {
        Self(SOCKET_ALLOCATOR.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    }
}

/// Struct sent over the socket once created to pass metadata to the hook
#[derive(Debug)]
pub struct SocketInformation {
    /// Address of the incoming peer
    pub remote_address: SocketAddr,
    /// Address of the local peer (our IP)
    pub local_address: SocketAddr,
}

/// poll_agent loop inserts connection data into this queue, and accept reads it.
#[derive(Debug, Default)]
pub struct ConnectionQueue {
    connections: HashMap<SocketId, VecDeque<SocketInformation>>,
}

impl ConnectionQueue {
    pub fn add(&mut self, id: SocketId, info: SocketInformation) {
        self.connections.entry(id).or_default().push_back(info);
    }

    pub fn get(&mut self, id: SocketId) -> Option<SocketInformation> {
        let mut queue = self.connections.remove(&id)?;
        if let Some(info) = queue.pop_front() {
            if !queue.is_empty() {
                self.connections.insert(id, queue);
            }
            Some(info)
        } else {
            None
        }
    }
}

impl SocketInformation {
    pub fn new(remote_address: SocketAddr, local_address: SocketAddr) -> Self {
        Self {
            remote_address,
            local_address,
        }
    }
}

#[derive(Debug)]
pub struct Connected {
    /// Remote address we're connected to
    remote_address: SocketAddr,
    /// Local address (pod-wise)
    local_address: SocketAddr,
}

#[derive(Debug, Clone, Copy)]
pub struct Bound {
    requested_port: Port,
    address: SocketAddr,
}

#[derive(Debug, Default)]
pub enum SocketState {
    #[default]
    Initialized,
    Bound(Bound),
    Listening(Bound),
    Connected(Connected),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketKind {
    Tcp(c_int),
    Udp(c_int),
}

impl TryFrom<c_int> for SocketKind {
    type Error = Bypass;

    fn try_from(type_: c_int) -> Result<Self, Self::Error> {
        if (type_ & libc::SOCK_STREAM) > 0 {
            Ok(SocketKind::Tcp(type_))
        } else if (type_ & libc::SOCK_DGRAM) > 0 {
            Ok(SocketKind::Udp(type_))
        } else {
            Err(Bypass::Type(type_))
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct UserSocket {
    pub(crate) id: SocketId,
    domain: c_int,
    type_: c_int,
    protocol: c_int,
    pub state: SocketState,
    pub(crate) kind: SocketKind,
}

impl UserSocket {
    pub(crate) fn new(
        domain: c_int,
        type_: c_int,
        protocol: c_int,
        state: SocketState,
        kind: SocketKind,
    ) -> Self {
        Self {
            id: SocketId::new(),
            domain,
            type_,
            protocol,
            state,
            kind,
        }
    }
}

#[inline]
fn is_ignored_port(addr: SocketAddr) -> bool {
    let (ip, port) = (addr.ip(), addr.port());
    let ignored_ip = ip == IpAddr::V4(Ipv4Addr::LOCALHOST) || ip == IpAddr::V6(Ipv6Addr::LOCALHOST);
    port == 0 || ignored_ip && (port > 50000 && port < 60000)
}

/// Fill in the sockaddr structure for the given address.
#[inline]
fn fill_address(
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_address: SocketAddr,
) -> Detour<i32> {
    let result = if address.is_null() {
        Ok(0)
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

        Ok(0)
    }?;

    Detour::Success(result)
}

pub(crate) trait ProtocolExt {
    fn try_from_raw(ai_protocol: i32) -> HookResult<Protocol>;
    fn try_into_raw(self) -> HookResult<i32>;
}

impl ProtocolExt for Protocol {
    fn try_from_raw(ai_protocol: i32) -> HookResult<Self> {
        match ai_protocol {
            libc::IPPROTO_UDP => Ok(Protocol::Udp),
            libc::IPPROTO_TCP => Ok(Protocol::Tcp),
            libc::IPPROTO_SCTP => todo!(),
            other => {
                warn!("Trying a protocol of {:#?}", other);
                Ok(Protocol::Tcp)
            }
        }
    }

    fn try_into_raw(self) -> HookResult<i32> {
        match self {
            Protocol::Udp => Ok(libc::IPPROTO_UDP),
            Protocol::Tcp => Ok(libc::IPPROTO_TCP),
            _ => todo!(),
        }
    }
}

pub(crate) trait SocketAddrExt {
    fn try_from_raw(raw_address: *const sockaddr, address_length: socklen_t) -> Detour<SocketAddr>;
}

impl SocketAddrExt for SocketAddr {
    fn try_from_raw(raw_address: *const sockaddr, address_length: socklen_t) -> Detour<SocketAddr> {
        unsafe {
            SockAddr::init(|storage, len| {
                storage.copy_from_nonoverlapping(raw_address.cast(), 1);
                len.copy_from_nonoverlapping(&address_length, 1);

                Ok(())
            })
        }
        .ok()
        .and_then(|((), address)| address.as_socket())
        .bypass(Bypass::AddressConversion)
    }
}
