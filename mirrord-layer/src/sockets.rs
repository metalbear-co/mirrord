//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::{
    collections::{HashMap, VecDeque},
    ffi::{CStr, CString},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    ptr,
    sync::{Arc, LazyLock, Mutex},
};

use dns_lookup::AddrInfo;
use errno::{errno, set_errno, Errno};
use frida_gum::interceptor::Interceptor;
use libc::{c_char, c_int, sockaddr, socklen_t};
use mirrord_protocol::{AddrInfoHint, DaemonMessage, GetAddrInfoResponse, Port};
use os_socketaddr::OsSocketAddr;
use tokio::sync::oneshot;
use tracing::{debug, error, trace, warn};

use crate::{
    common::{GetAddrInfoHook, HookMessage},
    file::ops::blocking_send_hook_message,
    macros::{hook, try_hook},
    tcp::{HookMessageTcp, Listen},
    HOOK_SENDER,
};

pub(crate) static SOCKETS: LazyLock<Mutex<HashMap<RawFd, Arc<Socket>>>> =
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
pub struct Socket {
    domain: c_int,
    type_: c_int,
    protocol: c_int,
    pub state: SocketState,
}

#[inline]
fn is_ignored_port(port: Port) -> bool {
    port == 0 || (port > 50000 && port < 60000)
}

/// Create the socket, add it to SOCKETS if successful and matching protocol and domain (Tcpv4/v6)
fn socket(domain: c_int, type_: c_int, protocol: c_int) -> RawFd {
    debug!("socket called domain:{:?}, type:{:?}", domain, type_);
    let fd = unsafe { libc::socket(domain, type_, protocol) };
    if fd == -1 {
        error!("socket failed");
        return fd;
    }
    // We don't handle non Tcpv4 sockets
    if !((domain == libc::AF_INET) || (domain == libc::AF_INET6) && (type_ & libc::SOCK_STREAM) > 0)
    {
        debug!("non Tcp socket domain:{:?}, type:{:?}", domain, type_);
        return fd;
    }
    let mut sockets = SOCKETS.lock().unwrap();
    sockets.insert(
        fd,
        Arc::new(Socket {
            domain,
            type_,
            protocol,
            state: SocketState::default(),
        }),
    );
    fd
}

unsafe extern "C" fn socket_detour(domain: c_int, type_: c_int, protocol: c_int) -> c_int {
    socket(domain, type_, protocol)
}

/// Check if the socket is managed by us, if it's managed by us and it's not an ignored port,
/// update the socket state and don't call bind (will be called later). In any other case, we call
/// regular bind.
#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
fn bind(sockfd: c_int, addr: *const sockaddr, addrlen: socklen_t) -> c_int {
    debug!("bind called sockfd: {:?}", sockfd);
    let mut socket = {
        let mut sockets = SOCKETS.lock().unwrap();
        match sockets.remove(&sockfd) {
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

    Arc::get_mut(&mut socket).unwrap().state = SocketState::Bound(Bound {
        address: parsed_addr,
    });

    let mut sockets = SOCKETS.lock().unwrap();
    sockets.insert(sockfd, socket);
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
#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
fn listen(sockfd: RawFd, _backlog: c_int) -> c_int {
    debug!("listen called");
    let mut socket = {
        let mut sockets = SOCKETS.lock().unwrap();
        match sockets.remove(&sockfd) {
            Some(socket) => socket,
            None => {
                debug!("listen: no socket found for fd: {}", &sockfd);
                return unsafe { libc::listen(sockfd, _backlog) };
            }
        }
    };
    match &socket.state {
        SocketState::Bound(bound) => {
            let real_port = bound.address.port();
            Arc::get_mut(&mut socket).unwrap().state = SocketState::Listening(*bound);
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
            match sender.blocking_send(HookMessage::Tcp(HookMessageTcp::Listen(Listen {
                fake_port: result_addr.port(),
                real_port,
                ipv6: result_addr.is_ipv6(),
                fd: sockfd,
            }))) {
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
    sockets.insert(sockfd, socket);
    0
}

unsafe extern "C" fn listen_detour(sockfd: RawFd, backlog: c_int) -> c_int {
    listen(sockfd, backlog)
}

#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
fn connect(sockfd: RawFd, address: *const sockaddr, len: socklen_t) -> c_int {
    debug!("connect -> sockfd {:#?} | len {:#?}", sockfd, len);

    let socket = {
        let mut sockets = SOCKETS.lock().unwrap();

        match sockets.remove(&sockfd) {
            Some(socket) => socket,
            None => {
                warn!("connect: no socket found for fd: {}", &sockfd);
                return unsafe { libc::connect(sockfd, address, len) };
            }
        }
    };

    // We don't handle this socket, so restore state if there was any. (delay execute bind)
    if let SocketState::Bound(bound) = &socket.state {
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
#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
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
#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
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
    let (local_address, domain, protocol, type_) = {
        if let Some(socket) = SOCKETS.lock().unwrap().get(&sockfd) {
            if let SocketState::Listening(bound) = &socket.state {
                (bound.address, socket.domain, socket.protocol, socket.type_)
            } else {
                error!("original socket is not listening");
                return new_fd;
            }
        } else {
            debug!("origin socket not found");
            return new_fd;
        }
    };
    let socket_info = { CONNECTION_QUEUE.lock().unwrap().get(&sockfd) };
    let remote_address = match socket_info {
        Some(socket_info) => socket_info,
        None => {
            debug!("accept: socketinformation not found, probably not ours");
            return new_fd;
        }
    }
    .address;
    let new_socket = Socket {
        domain,
        protocol,
        type_,
        state: SocketState::Connected(Connected {
            remote_address,
            local_address,
        }),
    };
    fill_address(address, address_len, remote_address);

    SOCKETS.lock().unwrap().insert(new_fd, Arc::new(new_socket));
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

fn fcntl(orig_fd: c_int, cmd: c_int, fcntl_fd: i32) -> c_int {
    if fcntl_fd == -1 {
        error!("fcntl failed");
        return fcntl_fd;
    }
    match cmd {
        libc::F_DUPFD | libc::F_DUPFD_CLOEXEC => {
            dup(orig_fd, fcntl_fd);
        }
        _ => (),
    }
    fcntl_fd
}

unsafe extern "C" fn fcntl_detour(fd: c_int, cmd: c_int, arg: ...) -> c_int {
    let fcntl_fd = libc::fcntl(fd, cmd, arg);
    fcntl(fd, cmd, fcntl_fd)
}

fn dup(fd: c_int, dup_fd: i32) -> c_int {
    if dup_fd == -1 {
        error!("dup failed");
        return dup_fd;
    }
    let mut sockets = SOCKETS.lock().unwrap();
    if let Some(socket) = sockets.get(&fd) {
        let dup_socket = socket.clone();
        sockets.insert(dup_fd as RawFd, dup_socket);
    }
    dup_fd
}

unsafe extern "C" fn dup_detour(fd: c_int) -> c_int {
    let dup_fd = libc::dup(fd);
    dup(fd, dup_fd)
}

unsafe extern "C" fn dup2_detour(oldfd: c_int, newfd: c_int) -> c_int {
    if oldfd == newfd {
        return newfd;
    }
    let dup2_fd = libc::dup2(oldfd, newfd);
    dup(oldfd, dup2_fd)
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn dup3_detour(oldfd: c_int, newfd: c_int, flags: c_int) -> c_int {
    let dup3_fd = libc::dup3(oldfd, newfd, flags);
    dup(oldfd, dup3_fd)
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

/// # WARNING:
/// - `raw_hostname`, `raw_servname`, and/or `raw_hints` might be null!
unsafe extern "C" fn getaddrinfo_detour(
    raw_node: *const c_char,
    raw_service: *const c_char,
    raw_hints: *const libc::addrinfo,
    out_addr_info: *mut *mut libc::addrinfo,
) -> c_int {
    trace!(
        "getaddrinfo_detour -> raw_node {:#?} | raw_service {:#?} | raw_hints {:#?}",
        raw_node,
        raw_service,
        *raw_hints,
    );

    let node = match (raw_node.is_null() == false)
        .then(|| CStr::from_ptr(raw_node).to_str())
        .transpose()
        .map_err(|fail| {
            error!("Failed converting raw_node from `c_char` with {:#?}", fail);

            libc::EAI_MEMORY
        }) {
        Ok(node) => node.map(String::from),
        Err(fail) => return fail,
    };

    let service = match (raw_service.is_null() == false)
        .then(|| CStr::from_ptr(raw_service).to_str())
        .transpose()
        .map_err(|fail| {
            error!(
                "Failed converting raw_service from `c_char` with {:#?}",
                fail
            );

            libc::EAI_MEMORY
        }) {
        Ok(service) => service.map(String::from),
        Err(fail) => return fail,
    };

    let hints = (raw_hints.is_null() == false).then(|| AddrInfoHint::from_raw(*raw_hints));

    debug!(
        "getaddrinfo_detour -> node {:#?} | service {:#?} | hints {:#?}",
        node, service, hints
    );

    // TODO(alex) [high] 2022-07-01: Finish this implementation, first off try it out to see if
    // it works. Then replace this tuple struct with a proper struct that contains more data
    // (possibly even send the AddrInfo struct to -agent).
    let (hook_channel_tx, hook_channel_rx) = oneshot::channel::<GetAddrInfoResponse>();
    let hook = GetAddrInfoHook {
        node,
        service,
        hints,
        hook_channel_tx,
    };

    blocking_send_hook_message(HookMessage::GetAddrInfoHook(hook)).unwrap();

    let GetAddrInfoResponse(addr_info_list) = hook_channel_rx.blocking_recv().unwrap();

    // TODO(alex) [high] 2022-07-05: This "works" (crashes with invalid pointer), and to fix it
    // the pointer allocation must outlive this whole mess. Everything here is pretty ad-hoc,
    // refactoring into some outer functions and so on is a must.
    for result in addr_info_list
        .into_iter()
        .map(|addr_info_result| addr_info_result.map(AddrInfo::from))
    {
        match result {
            Ok(addr_info) => {
                trace!("Have a proper addr_info {:#?}", addr_info);
                let AddrInfo {
                    socktype,
                    protocol,
                    address,
                    sockaddr,
                    canonname,
                    flags,
                } = addr_info;

                let sockaddr = socket2::SockAddr::from(sockaddr);
                let canonname = canonname.map(CString::new).transpose().unwrap();
                let name = canonname
                    .as_ref()
                    .map_or_else(|| ptr::null(), |s| s.as_ptr());

                let mut raw_addr_info = libc::addrinfo {
                    ai_flags: flags,
                    ai_family: address,
                    ai_socktype: socktype,
                    ai_protocol: protocol,
                    ai_addrlen: sockaddr.len(),
                    ai_addr: sockaddr.as_ptr() as *mut _,
                    ai_canonname: name as *mut _,
                    ai_next: ptr::null_mut(),
                };

                let mut x: *mut _ = &mut raw_addr_info;
                let x: *mut *mut _ = &mut x;

                *out_addr_info = *x;

                return 0;
            }
            Err(fail) => error!("Have an error in addrinfo {:#?}", fail),
        }
    }

    todo!()
}

pub fn enable_socket_hooks(interceptor: &mut Interceptor) {
    hook!(interceptor, "socket", socket_detour);
    hook!(interceptor, "bind", bind_detour);
    hook!(interceptor, "listen", listen_detour);
    hook!(interceptor, "connect", connect_detour);
    hook!(interceptor, "fcntl", fcntl_detour);
    hook!(interceptor, "dup", dup_detour);
    hook!(interceptor, "dup2", dup2_detour);
    try_hook!(interceptor, "getpeername", getpeername_detour);
    try_hook!(interceptor, "getsockname", getsockname_detour);
    #[cfg(target_os = "linux")]
    {
        try_hook!(interceptor, "uv__accept4", accept4_detour);
        try_hook!(interceptor, "accept4", accept4_detour);
        try_hook!(interceptor, "dup3", dup3_detour);
    }
    try_hook!(interceptor, "accept", accept_detour);
    hook!(interceptor, "getaddrinfo", getaddrinfo_detour);
}
