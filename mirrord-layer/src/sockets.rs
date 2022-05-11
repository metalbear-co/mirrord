//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::{
    borrow::Borrow,
    collections::HashSet,
    hash::{Hash, Hasher},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::io::RawFd,
    sync::Mutex,
};

use errno::errno;
use frida_gum::interceptor::Interceptor;
use lazy_static::lazy_static;
use libc::{c_int, sockaddr, socklen_t};
use os_socketaddr::OsSocketAddr;
use tracing::{debug, error};

use crate::{
    common::{HookMessage, Listen, Port},
    macros::hook,
    HOOK_SENDER,
};

#[derive(Debug)]
pub struct Bound {
    address: SocketAddr,
}

#[derive(Debug)]
pub enum SocketState {
    Initialized,
    Bound(Bound),
    Listening,
}

impl Default for SocketState {
    fn default() -> Self {
        SocketState::Initialized
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct Socket {
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

lazy_static! {
    static ref SOCKETS: Mutex<HashSet<Socket>> = Mutex::new(HashSet::new());
}

#[inline]
fn is_ignored_port(port: Port) -> bool {
    port == 0 || (port > 50000 && port < 60000)
}

/// Create the socket, add it to SOCKETS if sucesssful and matching protocol and domain (TCPv4/v6)
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
            socket.state = SocketState::Listening;
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
                real_port: bound.address.port(),
                ipv6: result_addr.is_ipv6(),
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
    if let SocketState::Bound(Bound { address }) = socket.state {
        let os_addr = OsSocketAddr::from(address);
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

//     debug!("listen called");

//     match SOCKETS.set_connection_state(sockfd, ConnectionState::Listening) {
//         Ok(()) => {
//             let sender = NEW_CONNECTION_SENDER.lock().unwrap();
//             sender.as_ref().unwrap().blocking_send(sockfd).unwrap(); // Tell main thread to
// subscribe to agent             0
//         }
//         Err(()) => {
//             error!("Failed to set connection state to listening");
//             -1
//         }
//     }
// }

// unsafe extern "C" fn getpeername_detour(
//     sockfd: i32,
//     addr: *mut sockaddr,
//     addrlen: *mut socklen_t,
// ) -> i32 {
//     let socket_addr = SOCKETS.get_data_socket_address(sockfd).unwrap();
//     let os_addr: OsSocketAddr = socket_addr.into();
//     let len = std::cmp::min(*addrlen as usize, os_addr.len() as usize);
//     std::ptr::copy_nonoverlapping(os_addr.as_ptr() as *const u8, addr as *mut u8, len);

//     *addrlen = os_addr.len();
//     0
// }

// unsafe extern "C" fn setsockopt_detour(
//     _sockfd: i32,
//     _level: i32,
//     _optname: i32,
//     _optval: *mut c_char,
//     _optlen: socklen_t,
// ) -> i32 {
//     0
// }

// unsafe extern "C" fn accept_detour(
//     sockfd: i32,
//     addr: *mut sockaddr,
//     addrlen: *mut socklen_t,
// ) -> i32 {
//     debug!(
//         "Accept called with sockfd {:?}, addr {:?}, addrlen {:?}",
//         &sockfd, &addr, &addrlen
//     );
//     let socket_addr = SOCKETS.get_connection_socket_address(sockfd).unwrap();

//     if !addr.is_null() {
//         debug!("received non-null address in accept");
//         let os_addr: OsSocketAddr = socket_addr.into();
//         std::ptr::copy_nonoverlapping(os_addr.as_ptr(), addr, os_addr.len() as usize);
//     }

//     let connection_id = SOCKETS.read_single_connection(sockfd);
//     SOCKETS.create_data_socket(connection_id, socket_addr)
// }

// unsafe extern "C" fn accept4_detour(
//     sockfd: i32,
//     addr: *mut sockaddr,
//     addrlen: *mut socklen_t,
//     _flags: i32,
// ) -> i32 {
//     accept_detour(sockfd, addr, addrlen)
// }

pub fn enable_socket_hooks(interceptor: &mut Interceptor) {
    hook!(interceptor, "socket", socket_detour);
    hook!(interceptor, "bind", bind_detour);
    hook!(interceptor, "listen", listen_detour);
    hook!(interceptor, "connect", connect_detour);
    // hook!(interceptor, "getpeername", getpeername_detour);
    // hook!(interceptor, "setsockopt", setsockopt_detour);
    // try_hook!(interceptor, "uv__accept4", accept4_detour);
    // try_hook!(interceptor, "accept4", accept4_detour);
    // try_hook!(interceptor, "accept", accept_detour);
}
