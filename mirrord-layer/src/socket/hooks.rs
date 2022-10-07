use alloc::ffi::CString;
use core::{ffi::CStr, mem};
use std::os::unix::io::RawFd;

use frida_gum::interceptor::Interceptor;
use libc::{c_char, c_int, sockaddr, socklen_t};
use mirrord_macro::{hook_fn, hook_guard_fn};
use tracing::{error, info, trace};

use super::ops::*;
use crate::{detour::DetourGuard, replace};

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn socket_detour(
    domain: c_int,
    type_: c_int,
    protocol: c_int,
) -> c_int {
    let (Ok(result) | Err(result)) = socket(domain, type_, protocol)
        .bypass_with(|_| FN_SOCKET(domain, type_, protocol))
        .map_err(From::from)
        .inspect(|s| info!("{s:#?}"))
        .inspect_err(|e| error!("{e:#?}"));

    result
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn bind_detour(
    sockfd: c_int,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> c_int {
    let (Ok(result) | Err(result)) = bind(sockfd, raw_address, address_length)
        .bypass_with(|_| FN_BIND(sockfd, raw_address, address_length))
        .map_err(From::from);

    result
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn listen_detour(sockfd: RawFd, backlog: c_int) -> c_int {
    let (Ok(result) | Err(result)) = listen(sockfd, backlog)
        .bypass_with(|_| FN_LISTEN(sockfd, backlog))
        .map_err(From::from);

    result
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn connect_detour(
    sockfd: RawFd,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> c_int {
    let (Ok(result) | Err(result)) = connect(sockfd, raw_address, address_length)
        .bypass_with(|_| FN_CONNECT(sockfd, raw_address, address_length))
        .map_err(From::from);

    result
}

#[hook_guard_fn]
pub(super) unsafe extern "C" fn getpeername_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    let (Ok(result) | Err(result)) = getpeername(sockfd, address, address_len)
        .bypass_with(|_| FN_GETPEERNAME(sockfd, address, address_len))
        .map_err(From::from);

    result
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn getsockname_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    let (Ok(result) | Err(result)) = getsockname(sockfd, address, address_len)
        .bypass_with(|_| FN_GETSOCKNAME(sockfd, address, address_len))
        .map_err(From::from);

    result
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn accept_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    let accept_result = FN_ACCEPT(sockfd, address, address_len);

    if accept_result == -1 {
        accept_result
    } else {
        let (Ok(result) | Err(result)) = accept(sockfd, address, address_len, accept_result)
            .bypass(accept_result)
            .map_err(From::from);

        result
    }
}

#[cfg(target_os = "linux")]
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn accept4_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    flags: c_int,
) -> c_int {
    let accept_result = FN_ACCEPT4(sockfd, address, address_len, flags);

    if accept_result == -1 {
        accept_result
    } else {
        let (Ok(result) | Err(result)) = accept(sockfd, address, address_len, accept_result)
            .bypass(accept_result)
            .map_err(From::from);

        result
    }
}

#[cfg(target_os = "linux")]
#[hook_guard_fn]
#[allow(non_snake_case)]
pub(super) unsafe extern "C" fn uv__accept4_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    flags: c_int,
) -> c_int {
    trace!("uv__accept4_detour -> sockfd {:#?}", sockfd);

    accept4_detour(sockfd, address, address_len, flags)
}

/// https://github.com/metalbear-co/mirrord/issues/184
#[hook_fn]
pub(super) unsafe extern "C" fn fcntl_detour(fd: c_int, cmd: c_int, mut arg: ...) -> c_int {
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
            .bypass(fcntl_result)
            .map_err(From::from);

        trace!("fcntl_detour -> result {:#?}", result);
        result
    }
}

#[hook_guard_fn]
pub(super) unsafe extern "C" fn dup_detour(fd: c_int) -> c_int {
    let dup_result = FN_DUP(fd);

    if dup_result == -1 {
        dup_result
    } else {
        let (Ok(result) | Err(result)) = dup(fd, dup_result)
            .map(|()| dup_result)
            .bypass(dup_result)
            .map_err(From::from);

        trace!("dup_detour -> result {:#?}", result);
        result
    }
}

#[hook_guard_fn]
pub(super) unsafe extern "C" fn dup2_detour(oldfd: c_int, newfd: c_int) -> c_int {
    if oldfd == newfd {
        return newfd;
    }

    let dup2_result = FN_DUP2(oldfd, newfd);

    if dup2_result == -1 {
        dup2_result
    } else {
        let (Ok(result) | Err(result)) = dup(oldfd, dup2_result)
            .map(|()| dup2_result)
            .bypass(dup2_result)
            .map_err(From::from);

        trace!("dup2_detour -> result {:#?}", result);
        result
    }
}

#[cfg(target_os = "linux")]
#[hook_guard_fn]
pub(super) unsafe extern "C" fn dup3_detour(oldfd: c_int, newfd: c_int, flags: c_int) -> c_int {
    let dup3_result = FN_DUP3(oldfd, newfd, flags);

    if dup3_result == -1 {
        dup3_result
    } else {
        let (Ok(result) | Err(result)) = dup(oldfd, dup3_result)
            .map(|()| dup3_result)
            .bypass(dup3_result)
            .map_err(From::from);

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
    let rawish_node = (!raw_node.is_null()).then(|| CStr::from_ptr(raw_node));
    let rawish_service = (!raw_service.is_null()).then(|| CStr::from_ptr(raw_service));

    let (Ok(result) | Err(result)) =
        getaddrinfo(rawish_node, rawish_service, mem::transmute(raw_hints))
            .map(|c_addr_info_ptr| {
                out_addr_info.copy_from_nonoverlapping(&c_addr_info_ptr, 1);

                0
            })
            .bypass_with(|_| FN_GETADDRINFO(raw_node, raw_service, raw_hints, out_addr_info))
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
    // Iterate over `addrinfo` linked list dropping it.
    let mut current = addrinfo;
    while !current.is_null() {
        let current_box = Box::from_raw(current);
        let ai_addr = Box::from_raw(current_box.ai_addr);
        let ai_canonname = CString::from_raw(current_box.ai_canonname);

        current = (*current).ai_next;

        drop(ai_addr);
        drop(ai_canonname);
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
