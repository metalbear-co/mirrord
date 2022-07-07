use std::{
    ffi::{CStr, CString},
    mem,
    os::unix::io::RawFd,
    ptr,
};

use dns_lookup::AddrInfo;
use frida_gum::interceptor::Interceptor;
use libc::{c_char, c_int, sockaddr, socklen_t};
use mirrord_protocol::{AddrInfoHint, GetAddrInfoResponse};
use tokio::sync::oneshot;
use tracing::{debug, error, info, trace};

use super::ops::*;
use crate::{
    common::{blocking_send_hook_message, GetAddrInfoHook, HookMessage},
    macros::{hook, try_hook},
    socket::AddrInfoHintExt,
};

unsafe extern "C" fn socket_detour(domain: c_int, type_: c_int, protocol: c_int) -> c_int {
    socket(domain, type_, protocol)
}

unsafe extern "C" fn bind_detour(
    sockfd: c_int,
    addr: *const sockaddr,
    addrlen: socklen_t,
) -> c_int {
    bind(sockfd, addr, addrlen)
}

unsafe extern "C" fn listen_detour(sockfd: RawFd, backlog: c_int) -> c_int {
    listen(sockfd, backlog)
}

unsafe extern "C" fn connect_detour(
    sockfd: RawFd,
    address: *const sockaddr,
    len: socklen_t,
) -> c_int {
    connect(sockfd, address, len)
}

unsafe extern "C" fn getpeername_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> i32 {
    getpeername(sockfd, address, address_len)
}

unsafe extern "C" fn getsockname_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> i32 {
    getsockname(sockfd, address, address_len)
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

unsafe extern "C" fn fcntl_detour(fd: c_int, cmd: c_int, arg: ...) -> c_int {
    let fcntl_fd = libc::fcntl(fd, cmd, arg);
    fcntl(fd, cmd, fcntl_fd)
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

/// # WARNING:
/// - `raw_hostname`, `raw_servname`, and/or `raw_hints` might be null!
unsafe extern "C" fn getaddrinfo_detour(
    raw_node: *const c_char,
    raw_service: *const c_char,
    raw_hints: *const libc::addrinfo,
    out_addr_info: *mut *mut libc::addrinfo,
) -> c_int {
    trace!(
        "getaddrinfo_detour -> raw_node {:#?} | raw_service {:#?} | raw_hints {:#?} | out {:#?}",
        raw_node,
        raw_service,
        *raw_hints,
        out_addr_info.is_null(),
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

    let (hook_channel_tx, hook_channel_rx) = oneshot::channel::<GetAddrInfoResponse>();
    let hook = GetAddrInfoHook {
        node,
        service,
        hints,
        hook_channel_tx,
    };

    blocking_send_hook_message(HookMessage::GetAddrInfoHook(hook)).unwrap();

    let GetAddrInfoResponse(addr_info_list) = hook_channel_rx.blocking_recv().unwrap();

    let c_addr_info_list = addr_info_list
        .into_iter()
        .flat_map(|result| {
            result.map(AddrInfo::from).map(|addr_info| {
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
                let ai_canonname = canonname.map_or_else(
                    || ptr::null(),
                    |c_string| {
                        let c_str = c_string.as_c_str();
                        c_str.as_ptr()
                    },
                ) as *mut _;

                let c_addr_info = libc::addrinfo {
                    ai_flags: flags,
                    ai_family: address,
                    ai_socktype: socktype,
                    ai_protocol: protocol,
                    ai_addrlen: sockaddr.len(),
                    ai_addr: sockaddr.as_ptr() as *mut _,
                    ai_canonname,
                    ai_next: ptr::null_mut(),
                };

                info!("c_addr_info {:#?}", c_addr_info);

                c_addr_info
            })
        })
        .collect::<Vec<_>>();

    // Converts a `Vec<addrinfo>` into a C-style linked list.
    let mut c_addr_info_ptr = c_addr_info_list
        .into_iter()
        .rev()
        .map(Box::new)
        .map(Box::into_raw)
        .reduce(|current, mut previous| {
            info!("current {:#?} | previous {:#?}", current, previous);

            (*previous).ai_next = current;

            previous
        })
        .map_or_else(ptr::null_mut, |addr_info| addr_info);

    out_addr_info.copy_from_nonoverlapping(&mut c_addr_info_ptr, 1);

    // TODO(alex) [mid] 2022-07-07: Remove this (for debugging only).
    let mut current = *out_addr_info;
    while current.is_null() == false {
        info!("value is {:#?}", *current);

        current = (*current).ai_next;
    }

    0
}

/// No need to send any sort of `free` message to `mirrord-agent`, as the `addrinfo` there is not
/// kept around.
///
/// # WARN
///
/// The `addrinfo` pointer has to be allocated respecting the `Box`'s
/// [memory layout](https://doc.rust-lang.org/std/boxed/index.html#memory-layout).
unsafe extern "C" fn freeaddrinfo_detour(addrinfo: *mut libc::addrinfo) {
    trace!("freeaddrinfo_detour -> addrinfo {:#?}", *addrinfo);

    // TODO(alex) [mid] 2022-07-07: Remove this (for debugging only).
    let mut current = addrinfo;
    while current.is_null() == false {
        info!("value is {:#?}", *current);
        info!("addr is {:#?}", *(*current).ai_addr);

        current = (*current).ai_next;
    }

    // TODO(alex) [mid] 2022-07-07: Should we drop every allocation, or just the `addrinfo`
    // specified in the function argument?
    Box::from_raw(addrinfo);
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
    hook!(interceptor, "getaddrinfo", getaddrinfo_detour);
    hook!(interceptor, "freeaddrinfo", freeaddrinfo_detour);
}
