//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet, VecDeque},
    hash::{Hash, Hasher},
    lazy::SyncLazy,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    sync::Mutex,
};

use errno::{errno, set_errno, Errno};
use frida_gum::interceptor::Interceptor;
use libc::{c_int, sockaddr, socklen_t};
use os_socketaddr::OsSocketAddr;
use tracing::{debug, error};

use crate::{
    common::{HookMessage, Listen, Port},
    macros::{hook, try_hook},
    HOOK_SENDER,
};

pub(crate) static SOCKETS: SyncLazy<Mutex<HashSet<Socket>>> =
    SyncLazy::new(|| Mutex::new(HashSet::new()));

pub static CONNECTION_QUEUE: SyncLazy<Mutex<ConnectionQueue>> =
    SyncLazy::new(|| Mutex::new(ConnectionQueue::default()));

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

#[derive(Debug, Clone)]
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
pub(crate) struct Socket {
    fd: RawFd,
    domain: c_int,
    type_: c_int,
    protocol: c_int,
    pub state: SocketState,
}

impl PartialEq for Socket {
    fn eq(&self, other: &Self) -> bool {
        self.fd == other.fd
    }
}

impl Eq for Socket {}

impl Hash for Socket {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.fd.hash(state);
    }
}

impl Borrow<RawFd> for Socket {
    fn borrow(&self) -> &RawFd {
        &self.fd
    }
}

#[inline]
fn is_ignored_port(port: Port) -> bool {
    port == 0 || (port > 50000 && port < 60000)
}

/// Create the socket, add it to SOCKETS if successful and matching protocol and domain (TCPv4/v6)
fn socket(domain: c_int, type_: c_int, protocol: c_int) -> RawFd {
    debug!("socket called domain:{:?}, type:{:?}", domain, type_);
    let fd = unsafe { libc::socket(domain, type_, protocol) };
    if fd == -1 {
        error!("socket failed");
        return fd;
    }
    // We don't handle non TCPv4 sockets
    if !((domain == libc::AF_INET) || (domain == libc::AF_INET6) && (type_ & libc::SOCK_STREAM) > 0)
    {
        debug!("non TCP socket domain:{:?}, type:{:?}", domain, type_);
        return fd;
    }
    let mut sockets = SOCKETS.lock().unwrap();
    sockets.insert(Socket {
        fd,
        domain,
        type_,
        protocol,
        state: SocketState::default(),
    });
    fd
}

unsafe extern "C" fn socket_detour(domain: c_int, type_: c_int, protocol: c_int) -> c_int {
    socket(domain, type_, protocol)
}

/// Check if the socket is managed by us, if it's managed by us and it's not an ignored port,
/// update the socket state and don't call bind (will be called later). In any other case, we call
/// regular bind.
#[allow(clippy::significant_drop_in_scrutinee)] /// See https://github.com/rust-lang/rust-clippy/issues/8963
fn bind(sockfd: c_int, addr: *const sockaddr, addrlen: socklen_t) -> c_int {
    debug!("bind called sockfd: {:?}", sockfd);
    let mut socket = {
        let mut sockets = SOCKETS.lock().unwrap();
        match sockets.take(&sockfd) {
            Some(socket) if !matches!(socket.state, SocketState::Initialized) => {
                error!("socket is in invalid state for bind {:?}", socket.state);
                return libc::EINVAL;
            }
            Some(socket) => socket,
            None => {
                debug!("bind: no socket found for fd: {}", &sockfd);
                return unsafe { libc::bind(sockfd, addr, addrlen) };
            }
        }
    };

    let raw_addr = unsafe { OsSocketAddr::from_raw_parts(addr as *const u8, addrlen as usize) };
    let parsed_addr = match raw_addr.into_addr() {
        Some(addr) => addr,
        None => {
            error!("bind: failed to parse addr");
            return libc::EINVAL;
        }
    };

    debug!("bind:port: {}", parsed_addr.port());
    if is_ignored_port(parsed_addr.port()) {
        debug!("bind: ignoring port: {}", parsed_addr.port());
        return unsafe { libc::bind(sockfd, addr, addrlen) };
    }

    socket.state = SocketState::Bound(Bound {
        address: parsed_addr,
    });

    let mut sockets = SOCKETS.lock().unwrap();
    sockets.insert(socket);
    0
}

unsafe extern "C" fn bind_detour(
    sockfd: c_int,
    addr: *const sockaddr,
    addrlen: socklen_t,
) -> c_int {
    bind(sockfd, addr, addrlen)
}

/// Bind the socket to a fake, local port, and subscribe to the agent on the real port.
/// Messages received from the agent on the real port will later be routed to the fake local port.
#[allow(clippy::significant_drop_in_scrutinee)] /// See https://github.com/rust-lang/rust-clippy/issues/8963
fn listen(sockfd: RawFd, _backlog: c_int) -> c_int {
    debug!("listen called");
    let mut socket = {
        let mut sockets = SOCKETS.lock().unwrap();
        match sockets.take(&sockfd) {
            Some(socket) => socket,
            None => {
                debug!("listen: no socket found for fd: {}", &sockfd);
                return unsafe { libc::listen(sockfd, _backlog) };
            }
        }
    };
    match socket.state {
        SocketState::Bound(bound) => {
            let real_port = bound.address.port();
            socket.state = SocketState::Listening(bound);
            let mut os_addr = match socket.domain {
                libc::AF_INET => {
                    OsSocketAddr::from(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
                }
                libc::AF_INET6 => {
                    OsSocketAddr::from(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0))
                }
                _ => {
                    // shouldn't happen
                    debug!("unsupported domain");
                    return libc::EINVAL;
                }
            };

            let ret = unsafe { libc::bind(sockfd, os_addr.as_ptr(), os_addr.len()) };
            if ret != 0 {
                error!(
                    "listen: failed to bind socket ret: {:?}, addr: {:?}, sockfd: {:?}, errno: {:?}",
                    ret, os_addr, sockfd, errno()
                );
                return ret;
            }
            let mut addr_len = os_addr.len();
            // We need to find out what's the port we bound to, that'll be used by `poll_agent` to
            // connect to.
            let ret = unsafe { libc::getsockname(sockfd, os_addr.as_mut_ptr(), &mut addr_len) };
            if ret != 0 {
                error!(
                    "listen: failed to get sockname ret: {:?}, addr: {:?}, sockfd: {:?}",
                    ret, os_addr, sockfd
                );
                return ret;
            }
            let result_addr = match os_addr.into_addr() {
                Some(addr) => addr,
                None => {
                    error!("listen: failed to parse addr");
                    return libc::EINVAL;
                }
            };
            let ret = unsafe { libc::listen(sockfd, _backlog) };
            if ret != 0 {
                error!(
                    "listen: failed to listen ret: {:?}, addr: {:?}, sockfd: {:?}",
                    ret, result_addr, sockfd
                );
                return ret;
            }
            let sender = unsafe { HOOK_SENDER.as_ref().unwrap() };
            match sender.blocking_send(HookMessage::Listen(Listen {
                fake_port: result_addr.port(),
                real_port,
                ipv6: result_addr.is_ipv6(),
                fd: sockfd,
            })) {
                Ok(_) => {}
                Err(e) => {
                    error!("listen: failed to send listen message: {:?}", e);
                    return libc::EFAULT;
                }
            };
        }
        _ => {
            error!(
                "listen: socket is not bound or already listening, state: {:?}",
                socket.state
            );
            return libc::EINVAL;
        }
    }
    debug!("listen: success");
    let mut sockets = SOCKETS.lock().unwrap();
    sockets.insert(socket);
    0
}

unsafe extern "C" fn listen_detour(sockfd: RawFd, backlog: c_int) -> c_int {
    listen(sockfd, backlog)
}

#[allow(clippy::significant_drop_in_scrutinee)] /// See https://github.com/rust-lang/rust-clippy/issues/8963
fn connect(sockfd: RawFd, address: *const sockaddr, len: socklen_t) -> c_int {
    debug!("connect called");

    let socket = {
        let mut sockets = SOCKETS.lock().unwrap();
        match sockets.take(&sockfd) {
            Some(socket) => socket,
            None => {
                debug!("connect: no socket found for fd: {}", &sockfd);
                return unsafe { libc::connect(sockfd, address, len) };
            }
        }
    };

    // We don't handle this socket, so restore state if there was any. (delay execute bind)
    if let SocketState::Bound(bound) = socket.state {
        let os_addr = OsSocketAddr::from(bound.address);
        let ret = unsafe { libc::bind(sockfd, os_addr.as_ptr(), os_addr.len()) };
        if ret != 0 {
            error!(
                "connect: failed to bind socket ret: {:?}, addr: {:?}, sockfd: {:?}",
                ret, os_addr, sockfd
            );
            return ret;
        }
    };
    unsafe { libc::connect(sockfd, address, len) }
}

unsafe extern "C" fn connect_detour(
    sockfd: RawFd,
    address: *const sockaddr,
    len: socklen_t,
) -> c_int {
    connect(sockfd, address, len)
}

/// Resolve fake local address to real remote address. (IP & port of incoming traffic on the
/// cluster)
#[allow(clippy::significant_drop_in_scrutinee)] /// See https://github.com/rust-lang/rust-clippy/issues/8963
fn getpeername(sockfd: RawFd, address: *mut sockaddr, address_len: *mut socklen_t) -> c_int {
    debug!("getpeername called");
    let remote_address = {
        let sockets = SOCKETS.lock().unwrap();
        match sockets.get(&sockfd) {
            Some(socket) => match &socket.state {
                SocketState::Connected(connected) => connected.remote_address,
                _ => {
                    debug!(
                        "getpeername: socket is not connected, state: {:?}",
                        socket.state
                    );
                    set_errno(Errno(libc::ENOTCONN));
                    return -1;
                }
            },
            None => {
                debug!("getpeername: no socket found for fd: {}", &sockfd);
                return unsafe { libc::getpeername(sockfd, address, address_len) };
            }
        }
    };
    debug!("remote_address: {:?}", remote_address);
    fill_address(address, address_len, remote_address)
}

unsafe extern "C" fn getpeername_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> i32 {
    getpeername(sockfd, address, address_len)
}

/// Resolve the fake local address to the real local address.
#[allow(clippy::significant_drop_in_scrutinee)] /// See https://github.com/rust-lang/rust-clippy/issues/8963
fn getsockname(sockfd: RawFd, address: *mut sockaddr, address_len: *mut socklen_t) -> c_int {
    debug!("getsockname called");
    let local_address = {
        let sockets = SOCKETS.lock().unwrap();
        match sockets.get(&sockfd) {
            Some(socket) => match &socket.state {
                SocketState::Connected(connected) => connected.local_address,
                SocketState::Bound(bound) => bound.address,
                SocketState::Listening(bound) => bound.address,
                _ => {
                    debug!(
                        "getsockname: socket is not bound or connected, state: {:?}",
                        socket.state
                    );
                    return unsafe { libc::getsockname(sockfd, address, address_len) };
                }
            },
            None => {
                debug!("getsockname: no socket found for fd: {}", &sockfd);
                return unsafe { libc::getsockname(sockfd, address, address_len) };
            }
        }
    };
    debug!("local_address: {:?}", local_address);
    fill_address(address, address_len, local_address)
}

unsafe extern "C" fn getsockname_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> i32 {
    getsockname(sockfd, address, address_len)
}

/// Fill in the sockaddr structure for the given address.
#[inline]
fn fill_address(
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

/// When the fd is "ours", we accept and recv the first bytes that contain metadata on the
/// connection to be set in our lock This enables us to have a safe way to get "remote" information
/// (remote ip, port, etc).
fn accept(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_fd: RawFd,
) -> RawFd {
    let (origin_fd, local_address, domain, protocol, type_) = {
        if let Some(socket) = SOCKETS.lock().unwrap().get(&sockfd) {
            if let SocketState::Listening(bound) = &socket.state {
                (
                    socket.fd,
                    bound.address,
                    socket.domain,
                    socket.protocol,
                    socket.type_,
                )
            } else {
                error!("original socket is not listening");
                return new_fd;
            }
        } else {
            debug!("origin socket not found");
            return new_fd;
        }
    };
    let socket_info = { CONNECTION_QUEUE.lock().unwrap().get(&origin_fd) };
    let remote_address = match socket_info {
        Some(socket_info) => socket_info,
        None => {
            debug!("accept: socketinformation not found, probably not ours");
            return new_fd;
        }
    }
    .address;
    let new_socket = Socket {
        fd: new_fd,
        domain,
        protocol,
        type_,
        state: SocketState::Connected(Connected {
            remote_address,
            local_address,
        }),
    };
    fill_address(address, address_len, remote_address);

    SOCKETS.lock().unwrap().insert(new_socket);
    new_fd
}

unsafe extern "C" fn accept_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> i32 {
    let accept_fd = libc::accept(sockfd, address, address_len);

    if accept_fd == -1 {
        accept_fd
    } else {
        accept(sockfd, address, address_len, accept_fd)
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn accept4_detour(
    sockfd: i32,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    flags: i32,
) -> i32 {
    let accept_fd = libc::accept4(sockfd, address, address_len, flags);

    if accept_fd == -1 {
        accept_fd
    } else {
        accept(sockfd, address, address_len, accept_fd)
    }
}

pub fn enable_socket_hooks(interceptor: &mut Interceptor) {
    hook!(interceptor, "socket", socket_detour);
    hook!(interceptor, "bind", bind_detour);
    hook!(interceptor, "listen", listen_detour);
    hook!(interceptor, "connect", connect_detour);
    try_hook!(interceptor, "getpeername", getpeername_detour);
    try_hook!(interceptor, "getsockname", getsockname_detour);
    #[cfg(target_os = "linux")]
    {
        try_hook!(interceptor, "uv__accept4", accept4_detour);
        try_hook!(interceptor, "accept4", accept4_detour);
    }
    try_hook!(interceptor, "accept", accept_detour);
}
