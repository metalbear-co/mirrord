use std::os::unix::io::RawFd;

use frida_gum::interceptor::Interceptor;
use libc::{c_int, c_void, sockaddr, socklen_t};
use os_socketaddr::OsSocketAddr;
use socket2::{Domain, SockAddr, Type};
use tracing::{error, info, trace};

use super::ops::*;
use crate::{
    error::LayerError,
    macros::{hook, try_hook},
    socket::MIRROR_SOCKETS,
};

pub(super) unsafe extern "C" fn socket_detour(
    domain: c_int,
    type_: c_int,
    protocol: c_int,
) -> c_int {
    trace!(
        "socket_detour -> domain {:#?} | type_ {:#?} | protocol {:#?}",
        domain,
        type_,
        protocol
    );

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
    trace!(
        "bind_detour -> sockfd {:#?} | addrlen {:#?}",
        sockfd,
        addrlen
    );

    let address = OsSocketAddr::from_raw_parts(addr as *const u8, addrlen as usize)
        .try_into()
        .unwrap();

    bind(sockfd, address)
        .map(|()| 0)
        .map_err(|fail| {
            error!("Failed bind call with {:#?}!", fail);

            match fail {
                LayerError::LocalFdNotFound(_) | LayerError::BypassBind(_) => {
                    libc::bind(sockfd, addr, addrlen)
                }
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

pub(super) unsafe extern "C" fn listen_detour(sockfd: RawFd, backlog: c_int) -> c_int {
    trace!(
        "listen_detour -> sockfd {:#?} | backlog {:#?}",
        sockfd,
        backlog
    );

    listen(sockfd, backlog)
        .map(|()| 0)
        .map_err(|fail| {
            error!("Failed listen call with {:#?}!", fail);

            match fail {
                LayerError::LocalFdNotFound(_) => libc::listen(sockfd, backlog),
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

pub(super) unsafe extern "C" fn connect_detour(
    sockfd: RawFd,
    addr: *const sockaddr,
    addrlen: socklen_t,
) -> c_int {
    trace!(
        "connect_detour -> sockfd {:#?} | addrlen {:#?}",
        sockfd,
        addrlen
    );

    let address = OsSocketAddr::from_raw_parts(addr as *const u8, addrlen as usize)
        .try_into()
        .unwrap();

    connect(sockfd, address)
        .map(|()| 0)
        .map_err(|fail| {
            error!("Failed connect call with {:#?}!", fail);

            match fail {
                LayerError::LocalFdNotFound(_) => libc::connect(sockfd, addr, addrlen),
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

// TODO(alex) [high] 2022-06-24: Node crashes with `GetSockOrPeerName` call.
pub(super) unsafe extern "C" fn getpeername_detour(
    sockfd: RawFd,
    out_address: *mut sockaddr,
    out_address_len: *mut socklen_t,
) -> c_int {
    trace!("getpeername_detour -> sockfd {:#?}", sockfd,);

    getpeername(sockfd)
        .map(|address| {
            trace!(
                "getpeername_detour -> address {:?} | out {:?}",
                address,
                *out_address
            );

            // TODO(alex) [mid] 2022-06-27: Weirdness around here, the pointer copy looks completely
            // fine (values are what they should be), but we crash when someone tries to use the
            // address.

            // let address_ptr = address.as_ptr();
            // let address_len = (*out_address_len).min(address.len()) as usize;

            // out_address.copy_from_nonoverlapping(address_ptr, address_len);
            // *out_address_len = address.len();

            trace!(
                "getpeername_detour -> address {:?} | out {:?}",
                address,
                *out_address
            );
            libc::getpeername(sockfd, out_address, out_address_len)

            // 0
        })
        .map_err(|fail| {
            error!("Failed getpeername call with {:#?}!", fail);

            match fail {
                LayerError::LocalFdNotFound(_) => {
                    libc::getpeername(sockfd, out_address, out_address_len)
                }
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

pub(super) unsafe extern "C" fn getsockname_detour(
    sockfd: RawFd,
    out_address: *mut sockaddr,
    out_address_len: *mut socklen_t,
) -> c_int {
    trace!("getsockname_detour -> sockfd {:#?}", sockfd,);

    getsockname(sockfd)
        .map(|address| {
            trace!(
                "getsockname_detour -> address {:?} | ptr {:?} | out {:?}",
                address,
                *address.as_ptr(),
                *out_address
            );

            // TODO(alex) [mid] 2022-06-27: Weirdness around here, the pointer copy looks completely
            // fine (values are what they should be), but we crash when someone tries to use the
            // address.

            // let address_ptr = address.as_ptr();
            // let address_len = (*out_address_len).min(address.len()) as usize;

            // out_address.copy_from_nonoverlapping(address_ptr, address_len);
            // *out_address_len = address.len();

            // trace!(
            //     "getsockname_detour -> address {:#?} | ptr {:#?} | out {:#?}",
            //     address,
            //     *address.as_ptr(),
            //     *out_address
            // );

            // 0

            libc::getsockname(sockfd, out_address, out_address_len)
        })
        .map_err(|fail| {
            error!("Failed getsockname call with {:#?}!", fail);

            match fail {
                LayerError::LocalFdNotFound(_) => {
                    libc::getsockname(sockfd, out_address, out_address_len)
                }
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

pub(super) unsafe extern "C" fn accept_detour(
    sockfd: c_int,
    out_address: *mut sockaddr,
    out_address_len: *mut socklen_t,
) -> c_int {
    trace!("accept_detour -> sockfd {:#?}", sockfd);

    accept(sockfd, None)
        .map(|(accepted_fd, address)| {
            let address_ptr = address.as_ptr();
            out_address.copy_from(address_ptr, (*out_address_len) as usize);

            trace!(
                "accept_detour -> Copied pointer address {:#?}",
                *out_address
            );

            accepted_fd
        })
        .map_err(|fail| {
            error!("Failed accept call with {:#?}!", fail);

            match fail {
                LayerError::LocalFdNotFound(_) => {
                    libc::accept(sockfd, out_address, out_address_len)
                }
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

#[cfg(target_os = "linux")]
pub(super) unsafe extern "C" fn accept4_detour(
    sockfd: c_int,
    out_address: *mut sockaddr,
    out_address_len: *mut socklen_t,
    flags: c_int,
) -> c_int {
    trace!(
        "accept4_detour -> sockfd {:#?} | flags {:#?}",
        sockfd,
        flags
    );

    accept(sockfd, Some(flags))
        .map(|(accepted_fd, address)| {
            let address_ptr = address.as_ptr();
            out_address.copy_from(address_ptr, (*out_address_len) as usize);

            trace!(
                "accept4_detour -> Copied pointer address {:#?}",
                *out_address
            );

            accepted_fd
        })
        .map_err(|fail| {
            error!("Failed accept4 call with {:#?}!", fail);

            match fail {
                LayerError::LocalFdNotFound(_) => {
                    libc::accept4(sockfd, out_address, out_address_len, flags)
                }
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

pub(super) unsafe extern "C" fn dup_detour(sockfd: c_int) -> c_int {
    trace!("dup_detour -> sockfd {:#?}", sockfd,);

    dup(sockfd)
        .map_err(|fail| {
            error!("Failed dup call with {:#?}!", fail);

            match fail {
                LayerError::LocalFdNotFound(_) => libc::dup(sockfd),
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

pub(super) unsafe extern "C" fn dup2_detour(oldfd: c_int, newfd: c_int) -> c_int {
    trace!("dup2_detour -> oldfd {:#?} | newfd {:#?}", oldfd, newfd);

    dup(oldfd)
        .map_err(|fail| {
            error!("Failed dup2 call with {:#?}!", fail);

            match fail {
                LayerError::LocalFdNotFound(_) => libc::dup2(oldfd, newfd),
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

#[cfg(target_os = "linux")]
pub(super) unsafe extern "C" fn dup3_detour(oldfd: c_int, newfd: c_int, flags: c_int) -> c_int {
    trace!(
        "dup3_detour -> oldfd {:#?} | newfd {:#?} | flags {:#?}",
        oldfd,
        newfd,
        flags
    );

    dup(oldfd)
        .map_err(|fail| {
            error!("Failed dup3 call with {:#?}!", fail);

            match fail {
                LayerError::LocalFdNotFound(_) => libc::dup3(oldfd, newfd, flags),
                LayerError::IO(io_error) => io_error.raw_os_error().unwrap(),
                _ => -1,
            }
        })
        .unwrap_or_else(|fail| fail)
}

pub(super) unsafe extern "C" fn fcntl_detour(fd: c_int, cmd: c_int, arg: ...) -> c_int {
    trace!(
        "fcntl_detour -> fd {:#?} | cmd {:#?} | arg {:#?}",
        fd,
        cmd,
        arg
    );

    if libc::F_DUPFD == cmd || libc::F_DUPFD_CLOEXEC == cmd {
        dup_detour(fd)
    } else {
        libc::fcntl(fd, cmd, arg)
    }
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
