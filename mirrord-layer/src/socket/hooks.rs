use std::{os::unix::io::RawFd, sync::Arc};

use errno::{errno, set_errno, Errno};
use frida_gum::interceptor::Interceptor;
use libc::{c_int, sockaddr, socklen_t};
use os_socketaddr::OsSocketAddr;
use tracing::{error, trace, warn};

use super::{fill_address, ops::*, SocketState, MIRROR_SOCKETS};
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
            error!("Failed socket call with {:#?}!", fail);
            match fail {
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

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
            error!("Failed bind call with {:#?}!", fail);

            match fail {
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

pub(super) unsafe extern "C" fn listen_detour(sockfd: RawFd, backlog: c_int) -> c_int {
    trace!("listen_detour");

    listen(sockfd, backlog)
        .map(|()| 0)
        .map_err(|fail| {
            error!("Failed listen call with {:#?}!", fail);

            match fail {
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

pub(super) unsafe extern "C" fn connect_detour(
    sockfd: RawFd,
    address: *const sockaddr,
    len: socklen_t,
) -> c_int {
    trace!("connect_detour");

    let socket = {
        let mut sockets = MIRROR_SOCKETS.lock().unwrap();
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
    todo!()
}

pub(super) unsafe extern "C" fn getsockname_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> i32 {
    todo!()
}

pub(super) unsafe extern "C" fn accept_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> i32 {
    todo!()
}

#[cfg(target_os = "linux")]
pub(super) unsafe extern "C" fn accept4_detour(
    sockfd: i32,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    flags: i32,
) -> i32 {
    todo!()
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
