use std::{os::unix::io::RawFd, sync::Arc};

use errno::{errno, set_errno, Errno};
use frida_gum::interceptor::Interceptor;
use libc::{c_int, sockaddr, socklen_t};
use os_socketaddr::OsSocketAddr;
use tracing::{error, trace, warn};

use super::{fill_address, ops::*, SocketState, MANAGED_SOCKETS};
use crate::{
    error::LayerError,
    macros::{hook, try_hook},
};

pub(super) unsafe extern "C" fn socket_detour(
    domain: c_int,
    type_: c_int,
    protocol: c_int,
) -> c_int {
    trace!("socket_detour");

    socket(domain, type_, protocol)
        .map_err(|fail| {
            error!("Failed creating socket with {:#?}!", fail);
            match fail {
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

// TODO(alex) [high] 2022-06-22: Do the `bind` directly, instead of leaving it up to `listen`.
// ADD(alex) [high] 2022-06-23: Same for the other parts, lets create a more complete socket
// abstraction that grabs all the sockets.
pub(super) unsafe extern "C" fn bind_detour(
    sockfd: c_int,
    addr: *const sockaddr,
    addrlen: socklen_t,
) -> c_int {
    trace!("bind_detour");

    let address = OsSocketAddr::from_raw_parts(addr as *const u8, addrlen as usize)
        .try_into()
        .unwrap();

    bind(sockfd, address)
        .map(|()| 0)
        .map_err(|fail| {
            error!("Failed creating socket with {:#?}!", fail);
            match fail {
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

pub(super) unsafe extern "C" fn listen_detour(sockfd: RawFd, backlog: c_int) -> c_int {
    trace!("listen_detour");

    let socket = {
        let mut sockets = MANAGED_SOCKETS.lock().unwrap();
        sockets.remove(&sockfd)
    };

    if let Some(mut socket) = socket {
        if let SocketState::Bound(bound) = socket.state {
            let os_addr = OsSocketAddr::try_from(socket.as_ref());
            let mut os_addr = match os_addr {
                Ok(addr) => addr,
                Err(fail) => {
                    error!(
                        "listen_detour -> Failed converting into OsSocketAddr with {:#?}!",
                        fail
                    );
                    return -1;
                }
            };

            // TODO(alex) [low] 2022-06-22: Leaks, as we may fail after this call, so this `sockfd`
            // will remain bound forever.
            let bind_result = libc::bind(sockfd, os_addr.as_ptr(), os_addr.len());
            if bind_result != 0 {
                error!(
                    "listen_detour -> Failed bind {:#?} | addr {:#?} | sockfd: {:#?}, errno: {:?}!",
                    bind_result,
                    os_addr,
                    sockfd,
                    errno()
                );
                return bind_result;
            }

            let mut addr_len = os_addr.len();
            let getsockname_result = libc::getsockname(sockfd, os_addr.as_mut_ptr(), &mut addr_len);
            if getsockname_result != 0 {
                error!(
                    "listen_detour -> Failed to get sockname {:#?} | addr {:#?} | sockfd: {:#?}!",
                    getsockname_result, os_addr, sockfd
                );
                return getsockname_result;
            }

            let listen_result = libc::listen(sockfd, backlog);
            if listen_result != 0 {
                error!(
                    "listen_detour -> Failed to listen {:#?} | sockfd: {:#?}!",
                    listen_result, sockfd
                );
                return listen_result;
            }

            listen(Arc::get_mut(&mut socket).unwrap(), bound, os_addr, sockfd)
                .map(|()| {
                    MANAGED_SOCKETS.lock().unwrap().insert(sockfd, socket);
                    0
                })
                .map_err(|fail| {
                    error!("listen_detour -> Failed with {:#?}!", fail);
                    libc::EFAULT
                })
                .unwrap_or_else(|fail| fail)
        } else {
            error!(
                "listen_detour -> Failed socket is not bound or already listening, state: {:#?}!",
                socket.state
            );

            -1
        }
    } else {
        warn!("listen_detour -> No socket found for sockfd: {:#?}", sockfd);
        libc::listen(sockfd, backlog)
    }
}

pub(super) unsafe extern "C" fn connect_detour(
    sockfd: RawFd,
    address: *const sockaddr,
    len: socklen_t,
) -> c_int {
    trace!("connect_detour");

    let socket = {
        let mut sockets = MANAGED_SOCKETS.lock().unwrap();
        sockets.remove(&sockfd)
    };

    if let Some(socket) = socket {
        if let SocketState::Bound(bound) = socket.state {
            let os_addr = OsSocketAddr::from(bound.address);
            libc::bind(sockfd, os_addr.as_ptr(), os_addr.len())
        } else {
            error!(
                "connect_detour -> Socket {:#?} in invalid state for this call {:#?}!",
                sockfd, socket
            );

            -1
        }
    } else {
        warn!(
            "connect_detour -> No socket found for sockfd {:#?}",
            &sockfd
        );
        libc::connect(sockfd, address, len)
    }
}

pub(super) unsafe extern "C" fn getpeername_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    let sockets = MANAGED_SOCKETS.lock().unwrap();

    if let Some(socket) = sockets.get(&sockfd) {
        socket
            .get_connected_remote_address()
            .and_then(|remote_address| fill_address(address, address_len, remote_address))
            .map_err(|fail| {
                error!("getpeername_detour -> Failed with {:#?}!", fail);

                match fail {
                    LayerError::SocketInvalidState(_) => {
                        set_errno(Errno(libc::ENOTCONN));
                        -1
                    }
                    LayerError::NullSocketAddress => 0,
                    LayerError::NullAddressLength => {
                        set_errno(Errno(libc::EINVAL));
                        -1
                    }
                    _ => -1,
                }
            })
            .map(|()| 0)
            .unwrap_or_else(|fail| fail)
    } else {
        warn!(
            "getpeername_detour -> No socket found for sockfd {:#?}",
            sockfd
        );
        libc::getpeername(sockfd, address, address_len)
    }
}

pub(super) unsafe extern "C" fn getsockname_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> i32 {
    let sockets = MANAGED_SOCKETS.lock().unwrap();

    if let Some(socket) = sockets.get(&sockfd) {
        socket
            .get_local_address()
            .and_then(|remote_address| fill_address(address, address_len, remote_address))
            .map_err(|fail| {
                error!("getpeername_detour -> Failed with {:#?}!", fail);

                match fail {
                    LayerError::SocketInvalidState(_) => {
                        set_errno(Errno(libc::ENOTCONN));
                        -1
                    }
                    LayerError::NullSocketAddress => 0,
                    LayerError::NullAddressLength => {
                        set_errno(Errno(libc::EINVAL));
                        -1
                    }
                    _ => -1,
                }
            })
            .map(|()| 0)
            .unwrap_or_else(|fail| fail)
    } else {
        warn!(
            "getsockname_detour -> No socket found for sockfd {:#?}",
            sockfd
        );
        libc::getpeername(sockfd, address, address_len)
    }
}

pub(super) unsafe extern "C" fn accept_detour(
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
pub(super) unsafe extern "C" fn accept4_detour(
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

pub(super) unsafe extern "C" fn fcntl_detour(fd: c_int, cmd: c_int, arg: ...) -> c_int {
    let fcntl_fd = libc::fcntl(fd, cmd, arg);
    fcntl(fd, cmd, fcntl_fd)
}

pub(super) unsafe extern "C" fn dup_detour(fd: c_int) -> c_int {
    let dup_fd = libc::dup(fd);
    dup(fd, dup_fd)
}

pub(super) unsafe extern "C" fn dup2_detour(oldfd: c_int, newfd: c_int) -> c_int {
    if oldfd == newfd {
        return newfd;
    }
    let dup2_fd = libc::dup2(oldfd, newfd);
    dup(oldfd, dup2_fd)
}

#[cfg(target_os = "linux")]
pub(super) unsafe extern "C" fn dup3_detour(oldfd: c_int, newfd: c_int, flags: c_int) -> c_int {
    let dup3_fd = libc::dup3(oldfd, newfd, flags);
    dup(oldfd, dup3_fd)
}

pub(crate) fn enable_socket_hooks(interceptor: &mut Interceptor) {
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
}
