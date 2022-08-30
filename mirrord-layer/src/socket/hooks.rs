use std::{ffi::CStr, os::unix::io::RawFd};

use frida_gum::interceptor::Interceptor;
use libc::{c_char, c_int, sockaddr, socklen_t};
use mirrord_macro::{hook_fn, hook_guard_fn};
use mirrord_protocol::AddrInfoHint;
use socket2::SockAddr;
use tracing::{debug, error, trace, warn};

use super::ops::*;
use crate::{detour::DetourGuard, error::HookError, replace, socket::AddrInfoHintExt};

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn socket_detour(
    domain: c_int,
    type_: c_int,
    protocol: c_int,
) -> c_int {
    trace!(
        "socket_detour -> domain {:#?} | type:{:#?} | protocol {:#?}",
        domain,
        type_,
        protocol
    );

    let (Ok(result) | Err(result)) = socket(domain, type_, protocol).map_err(|fail| match fail {
        HookError::BypassedType(_) | HookError::BypassedDomain(_) => {
            warn!("socket_detour -> bypassed with {:#?}", fail);
            FN_SOCKET(domain, type_, protocol)
        }
        other => other.into(),
    });

    result
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn bind_detour(
    sockfd: c_int,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> c_int {
    trace!(
        "bind_detour -> sockfd {:#?} | raw_address {:#?}",
        sockfd,
        *raw_address
    );

    let address = SockAddr::init(|storage, len| {
        storage.copy_from_nonoverlapping(raw_address.cast(), 1);
        len.copy_from_nonoverlapping(&address_length, 1);

        Ok(())
    })
    .map_err(HookError::from);

    match address {
        Ok(((), address)) => {
            let (Ok(result) | Err(result)) =
                bind(sockfd, address)
                    .map(|()| 0)
                    .map_err(|fail| match fail {
                        HookError::LocalFDNotFound(_)
                        | HookError::BypassedPort(_)
                        | HookError::AddressConversion => {
                            warn!("bind_detour -> bypassed with {:#?}", fail);

                            FN_BIND(sockfd, raw_address, address_length)
                        }
                        other => other.into(),
                    });
            result
        }
        Err(_) => {
            warn!("bind_detour -> Could not convert address, bypassing!");

            FN_BIND(sockfd, raw_address, address_length)
        }
    }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn listen_detour(sockfd: RawFd, backlog: c_int) -> c_int {
    debug!(
        "listen_detour -> sockfd {:#?} | backlog {:#?}",
        sockfd, backlog
    );

    let (Ok(result) | Err(result)) =
        listen(sockfd, backlog)
            .map(|()| 0)
            .map_err(|fail| match fail {
                HookError::LocalFDNotFound(_) | HookError::SocketInvalidState(_) => {
                    warn!("listen_detour -> bypassed with {:#?}", fail);
                    FN_LISTEN(sockfd, backlog)
                }
                other => other.into(),
            });
    result
}

#[hook_guard_fn]
pub(super) unsafe extern "C" fn connect_detour(
    sockfd: RawFd,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> c_int {
    trace!("connect_detour -> sockfd {:#?}", sockfd);

    // TODO: Is this conversion safe?
    let address = SockAddr::new(*(raw_address as *const _), address_length);
    let address = address.as_socket().ok_or(HookError::AddressConversion);

    match address {
        Ok(address) => {
            let (Ok(result) | Err(result)) =
                connect(sockfd, address)
                    .map(|()| 0)
                    .map_err(|fail| match fail {
                        HookError::LocalFDNotFound(_) | HookError::SocketInvalidState(_) => {
                            warn!("connect_detour -> bypassed with {:#?}", fail);
                            FN_CONNECT(sockfd, raw_address, address_length)
                        }
                        other => other.into(),
                    });

            result
        }
        Err(_) => {
            warn!("connect_detour -> Could not convert address, bypassing!");

            FN_CONNECT(sockfd, raw_address, address_length)
        }
    }
}

#[hook_guard_fn]
pub(super) unsafe extern "C" fn getpeername_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    trace!("getpeername_detour -> sockfd {:#?}", sockfd);

    let (Ok(result) | Err(result)) = getpeername(sockfd, address, address_len)
        .map(|()| 0)
        .map_err(|fail| match fail {
            HookError::LocalFDNotFound(_) | HookError::SocketInvalidState(_) => {
                FN_GETPEERNAME(sockfd, address, address_len)
            }
            other => other.into(),
        });
    result
}

#[hook_guard_fn]
pub(super) unsafe extern "C" fn getsockname_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> i32 {
    trace!("getsockname_detour -> sockfd {:#?}", sockfd);

    let (Ok(result) | Err(result)) = getsockname(sockfd, address, address_len)
        .map(|()| 0)
        .map_err(|fail| match fail {
            HookError::LocalFDNotFound(_) | HookError::SocketInvalidState(_) => {
                FN_GETSOCKNAME(sockfd, address, address_len)
            }
            other => other.into(),
        });
    result
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn accept_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> i32 {
    trace!("accept_detour -> sockfd {:#?}", sockfd);

    let accept_result = FN_ACCEPT(sockfd, address, address_len);

    if accept_result == -1 {
        accept_result
    } else {
        let (Ok(result) | Err(result)) = accept(sockfd, address, address_len, accept_result)
            .map_err(|fail| match fail {
                HookError::SocketInvalidState(_) | HookError::LocalFDNotFound(_) => accept_result,
                other => {
                    error!("accept error is {:#?}", other);
                    other.into()
                }
            });

        result
    }
}

#[cfg(target_os = "linux")]
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn accept4_detour(
    sockfd: i32,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    flags: i32,
) -> i32 {
    trace!("accept4_detour -> sockfd {:#?}", sockfd);

    let accept_result = FN_ACCEPT4(sockfd, address, address_len, flags);

    if accept_result == -1 {
        accept_result
    } else {
        let (Ok(result) | Err(result)) = accept(sockfd, address, address_len, accept_result)
            .map_err(|fail| match fail {
                HookError::SocketInvalidState(_) | HookError::LocalFDNotFound(_) => accept_result,
                other => {
                    error!("accept4 error is {:#?}", other);
                    other.into()
                }
            });

        result
    }
}

#[cfg(target_os = "linux")]
#[hook_guard_fn]
#[allow(non_snake_case)]
pub(super) unsafe extern "C" fn uv__accept4_detour(
    sockfd: i32,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    flags: i32,
) -> i32 {
    trace!("uv__accept4_detour -> sockfd {:#?}", sockfd);

    accept4_detour(sockfd, address, address_len, flags)
}

/// https://github.com/metalbear-co/mirrord/issues/184
#[hook_fn]
pub(super) unsafe extern "C" fn fcntl_detour(fd: c_int, cmd: c_int, mut arg: ...) -> c_int {
    trace!("fcntl_detour -> fd {:#?} | cmd {:#?}", fd, cmd);

    let arg = arg.arg::<usize>();
    let fcntl_result = FN_FCNTL(fd, cmd, arg);
    let guard = DetourGuard::new();
    if guard.is_none() {
        return fcntl_result;
    }

    if fcntl_result == -1 {
        fcntl_result
    } else {
        let (Ok(result) | Err(result)) = fcntl(fd, cmd, fcntl_result)
            .map(|()| fcntl_result)
            .map_err(|fail| match fail {
                HookError::LocalFDNotFound(_) => fcntl_result,
                other => other.into(),
            });

        trace!("fcntl_detour -> result {:#?}", result);
        result
    }
}

#[hook_guard_fn]
pub(super) unsafe extern "C" fn dup_detour(fd: c_int) -> c_int {
    trace!("dup_detour -> fd {:#?}", fd);

    let dup_result = FN_DUP(fd);

    if dup_result == -1 {
        dup_result
    } else {
        let (Ok(result) | Err(result)) =
            dup(fd, dup_result)
                .map(|()| dup_result)
                .map_err(|fail| match fail {
                    HookError::LocalFDNotFound(_) => dup_result,
                    _ => fail.into(),
                });

        trace!("dup_detour -> result {:#?}", result);
        result
    }
}

#[hook_guard_fn]
pub(super) unsafe extern "C" fn dup2_detour(oldfd: c_int, newfd: c_int) -> c_int {
    trace!("dup2_detour -> oldfd {:#?} | newfd {:#?}", oldfd, newfd);

    if oldfd == newfd {
        return newfd;
    }

    let dup2_result = FN_DUP2(oldfd, newfd);

    if dup2_result == -1 {
        dup2_result
    } else {
        let (Ok(result) | Err(result)) =
            dup(oldfd, dup2_result)
                .map(|()| dup2_result)
                .map_err(|fail| match fail {
                    HookError::LocalFDNotFound(_) => dup2_result,
                    _ => fail.into(),
                });

        trace!("dup2_detour -> result {:#?}", result);
        result
    }
}

#[cfg(target_os = "linux")]
#[hook_guard_fn]
pub(super) unsafe extern "C" fn dup3_detour(oldfd: c_int, newfd: c_int, flags: c_int) -> c_int {
    trace!(
        "dup3_detour -> oldfd {:#?} | newfd {:#?} | flags {:#?}",
        oldfd,
        newfd,
        flags
    );

    let dup3_result = FN_DUP3(oldfd, newfd, flags);

    if dup3_result == -1 {
        dup3_result
    } else {
        let (Ok(result) | Err(result)) =
            dup(oldfd, dup3_result)
                .map(|()| dup3_result)
                .map_err(|fail| match fail {
                    HookError::LocalFDNotFound(_) => dup3_result,
                    _ => fail.into(),
                });

        trace!("dup3_detour -> result {:#?}", result);
        result
    }
}
/// Turns the raw pointer parameters into Rust types and calls `ops::getaddrinfo`.
///
/// # Warning:
/// - `raw_hostname`, `raw_servname`, and/or `raw_hints` might be null!
#[hook_guard_fn]
unsafe extern "C" fn getaddrinfo_detour(
    raw_node: *const c_char,
    raw_service: *const c_char,
    raw_hints: *const libc::addrinfo,
    out_addr_info: *mut *mut libc::addrinfo,
) -> c_int {
    trace!(
        "getaddrinfo_detour -> raw_node {:#?} | raw_service {:#?} | raw_hints {:#?} | out? {:#?}",
        raw_node,
        raw_service,
        *raw_hints,
        out_addr_info.is_null(),
    );

    let node = match (!raw_node.is_null())
        .then(|| CStr::from_ptr(raw_node).to_str())
        .transpose()
        .map_err(|fail| {
            error!("Failed converting raw_node from `c_char` with {:#?}", fail);

            libc::EAI_MEMORY
        }) {
        Ok(node) => node.map(String::from),
        Err(fail) => return fail,
    };

    let service = match (!raw_service.is_null())
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

    let hints = (!raw_hints.is_null()).then(|| AddrInfoHint::from_raw(*raw_hints));

    let (Ok(result) | Err(result)) = getaddrinfo(node, service, hints)
        .map(|c_addr_info_ptr| {
            out_addr_info.copy_from_nonoverlapping(&c_addr_info_ptr, 1);

            0
        })
        .map_err(From::from);

    result
}

/// Deallocates a `*mut libc::addrinfo` that was previously allocated with `Box::new` in
/// `getaddrinfo_detour` and converted into a raw pointer by `Box::into_raw`. Same thing must also
/// be done for `addrinfo.ai_addr`.
///
/// Also follows the `addr_info.ai_next` pointer, deallocating the next pointers in the linked list.
///
/// # Protocol
///
/// No need to send any sort of `free` message to `mirrord-agent`, as the `addrinfo` there is not
/// kept around.
///
/// # Warning
///
/// The `addrinfo` pointer has to be allocated respecting the `Box`'s
/// [memory layout](https://doc.rust-lang.org/std/boxed/index.html#memory-layout).
#[hook_guard_fn]
unsafe extern "C" fn freeaddrinfo_detour(addrinfo: *mut libc::addrinfo) {
    trace!("freeaddrinfo_detour -> addrinfo {:#?}", *addrinfo);

    // Iterate over `addrinfo` linked list dropping it.
    let mut current = addrinfo;
    while !current.is_null() {
        let current_box = Box::from_raw(current);
        let ai_addr = Box::from_raw(current_box.ai_addr);

        current = (*current).ai_next;

        drop(ai_addr);
        drop(current_box);
    }
}

pub(crate) unsafe fn enable_socket_hooks(interceptor: &mut Interceptor, enabled_remote_dns: bool) {
    let _ = replace!(interceptor, "socket", socket_detour, FnSocket, FN_SOCKET);

    let _ = replace!(interceptor, "bind", bind_detour, FnBind, FN_BIND);
    let _ = replace!(interceptor, "listen", listen_detour, FnListen, FN_LISTEN);

    let _ = replace!(
        interceptor,
        "connect",
        connect_detour,
        FnConnect,
        FN_CONNECT
    );

    let _ = replace!(interceptor, "fcntl", fcntl_detour, FnFcntl, FN_FCNTL);
    let _ = replace!(interceptor, "dup", dup_detour, FnDup, FN_DUP);
    let _ = replace!(interceptor, "dup2", dup2_detour, FnDup2, FN_DUP2);

    let _ = replace!(
        interceptor,
        "getpeername",
        getpeername_detour,
        FnGetpeername,
        FN_GETPEERNAME
    );

    let _ = replace!(
        interceptor,
        "getsockname",
        getsockname_detour,
        FnGetsockname,
        FN_GETSOCKNAME
    );

    #[cfg(target_os = "linux")]
    {
        let _ = replace!(
            interceptor,
            "uv__accept4",
            uv__accept4_detour,
            FnUv__accept4,
            FN_UV__ACCEPT4
        );

        let _ = replace!(
            interceptor,
            "accept4",
            accept4_detour,
            FnAccept4,
            FN_ACCEPT4
        );

        let _ = replace!(interceptor, "dup3", dup3_detour, FnDup3, FN_DUP3);
    }

    let _ = replace!(interceptor, "accept", accept_detour, FnAccept, FN_ACCEPT);

    if enabled_remote_dns {
        let _ = replace!(
            interceptor,
            "getaddrinfo",
            getaddrinfo_detour,
            FnGetaddrinfo,
            FN_GETADDRINFO
        );

        let _ = replace!(
            interceptor,
            "freeaddrinfo",
            freeaddrinfo_detour,
            FnFreeaddrinfo,
            FN_FREEADDRINFO
        );
    }
}
