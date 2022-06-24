use std::os::unix::io::RawFd;

use frida_gum::interceptor::Interceptor;
use libc::{c_int, sockaddr, socklen_t};
use os_socketaddr::OsSocketAddr;
use tracing::{error, trace};

use super::ops::*;
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
    trace!("listen_detour");

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
    getpeername(sockfd)
        .map(|address| {
            let address_ptr = address.as_ptr();
            out_address.copy_from(address_ptr, (*out_address_len) as usize);

            0
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
    getsockname(sockfd)
        .map(|address| {
            let address_ptr = address.as_ptr();
            out_address.copy_from(address_ptr, (*out_address_len) as usize);

            0
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
    accept(sockfd)
        .map(|(accepted_fd, address)| {
            let address_ptr = address.as_ptr();
            out_address.copy_from(address_ptr, (*out_address_len) as usize);

            accepted_fd
        })
        .map_err(|fail| {
            error!("Failed getsockname call with {:#?}!", fail);

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
    accept_detour(sockfd, out_address, out_address_len)
}

pub(super) unsafe extern "C" fn dup_detour(sockfd: c_int) -> c_int {
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
