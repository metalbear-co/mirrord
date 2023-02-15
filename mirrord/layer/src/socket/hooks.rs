use alloc::ffi::CString;
use core::{cmp, ffi::CStr, mem};
use std::{
    collections::HashSet,
    os::unix::io::RawFd,
    sync::{LazyLock, Mutex},
};

use errno::{set_errno, Errno};
use libc::{c_char, c_int, sockaddr, socklen_t, EINVAL};
use mirrord_layer_macro::{hook_fn, hook_guard_fn};

use super::ops::*;
use crate::{detour::DetourGuard, error::HookError, hooks::HookManager, replace};

/// Here we keep addr infos that we allocated so we'll know when to use the original
/// freeaddrinfo function and when to use our implementation
pub(crate) static MANAGED_ADDRINFO: LazyLock<Mutex<HashSet<usize>>> =
    LazyLock::new(|| Mutex::new(HashSet::with_capacity(4)));

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn socket_detour(
    domain: c_int,
    type_: c_int,
    protocol: c_int,
) -> c_int {
    socket(domain, type_, protocol).unwrap_or_bypass_with(|_| FN_SOCKET(domain, type_, protocol))
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn bind_detour(
    sockfd: c_int,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> c_int {
    bind(sockfd, raw_address, address_length)
        .unwrap_or_bypass_with(|_| FN_BIND(sockfd, raw_address, address_length))
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn listen_detour(sockfd: RawFd, backlog: c_int) -> c_int {
    listen(sockfd, backlog).unwrap_or_bypass_with(|_| FN_LISTEN(sockfd, backlog))
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn connect_detour(
    sockfd: RawFd,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> c_int {
    connect(sockfd, raw_address, address_length)
        .map(From::from)
        .unwrap_or_bypass_with(|_| FN_CONNECT(sockfd, raw_address, address_length))
}

#[hook_guard_fn]
pub(super) unsafe extern "C" fn getpeername_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    getpeername(sockfd, address, address_len)
        .unwrap_or_bypass_with(|_| FN_GETPEERNAME(sockfd, address, address_len))
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn getsockname_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    getsockname(sockfd, address, address_len)
        .unwrap_or_bypass_with(|_| FN_GETSOCKNAME(sockfd, address, address_len))
}

/// Hook for `libc::gethostname`.
///
/// Reads remote hostname bytes into `raw_name`, will rais EINVAL errno and return -1 if hostname
/// read more than `name_length`
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn gethostname_detour(
    raw_name: *mut c_char,
    name_length: usize,
) -> c_int {
    gethostname()
        .map(|host| {
            let host_len = host.as_bytes().len();

            raw_name.copy_from_nonoverlapping(host.as_ptr(), cmp::min(name_length, host_len));

            if host_len > name_length {
                set_errno(Errno(EINVAL));

                -1
            } else {
                0
            }
        })
        .unwrap_or_bypass_with(|_| FN_GETHOSTNAME(raw_name, name_length))
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
        accept(sockfd, address, address_len, accept_result).unwrap_or_bypass(accept_result)
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
        accept(sockfd, address, address_len, accept_result).unwrap_or_bypass(accept_result)
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
    tracing::trace!("uv__accept4_detour -> sockfd {:#?}", sockfd);

    accept4_detour(sockfd, address, address_len, flags)
}

/// <https://github.com/metalbear-co/mirrord/issues/184>
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
        match fcntl(fd, cmd, fcntl_result) {
            Ok(()) => fcntl_result,
            Err(e) => e.into(),
        }
    }
}

#[hook_guard_fn]
pub(super) unsafe extern "C" fn dup_detour(fd: c_int) -> c_int {
    let dup_result = FN_DUP(fd);

    if dup_result == -1 {
        dup_result
    } else {
        match dup(fd, dup_result) {
            Ok(()) => dup_result,
            Err(e) => e.into(),
        }
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
        match dup(oldfd, dup2_result) {
            Ok(()) => dup2_result,
            Err(e) => e.into(),
        }
    }
}

#[cfg(target_os = "linux")]
#[hook_guard_fn]
pub(super) unsafe extern "C" fn dup3_detour(oldfd: c_int, newfd: c_int, flags: c_int) -> c_int {
    let dup3_result = FN_DUP3(oldfd, newfd, flags);

    if dup3_result == -1 {
        dup3_result
    } else {
        match dup(oldfd, dup3_result) {
            Ok(()) => dup3_result,
            Err(e) => e.into(),
        }
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

    getaddrinfo(rawish_node, rawish_service, mem::transmute(raw_hints))
        .map(|c_addr_info_ptr| {
            out_addr_info.copy_from_nonoverlapping(&c_addr_info_ptr, 1);
            MANAGED_ADDRINFO
                .lock()
                .map(|mut managed_addrinfo| {
                    managed_addrinfo.insert(c_addr_info_ptr as usize);
                    0
                })
                .unwrap_or_else(|_| HookError::LockError.into())
        })
        .unwrap_or_bypass_with(|_| FN_GETADDRINFO(raw_node, raw_service, raw_hints, out_addr_info))
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
///
/// This needs to support trimmed linked lists, but at the moment if someone does that
/// it will call the original freeaddrinfo which might cause UB or crash.
/// if crashes occur on getaddrinfo - check this case.
/// This can be solved probably by adding each pointer in the linked list to our HashSet.
#[hook_guard_fn]
unsafe extern "C" fn freeaddrinfo_detour(addrinfo: *mut libc::addrinfo) {
    MANAGED_ADDRINFO
        .lock()
        .map(|mut managed_addrinfo| {
            managed_addrinfo
                .remove(&(addrinfo as usize))
                .then(|| {
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
                        managed_addrinfo.remove(&(current as usize));
                    }
                })
                .unwrap_or_else(|| {
                    // If the `addrinfo` pointer was not allocated by `getaddrinfo_detour`, then it
                    // is bypassed.
                    FN_FREEADDRINFO(addrinfo);
                })
        })
        .expect("Failed to lock MANAGED_ADDRINFO mutex");
}

pub(crate) unsafe fn enable_socket_hooks(hook_manager: &mut HookManager, enabled_remote_dns: bool) {
    replace!(hook_manager, "socket", socket_detour, FnSocket, FN_SOCKET);

    replace!(hook_manager, "bind", bind_detour, FnBind, FN_BIND);
    replace!(hook_manager, "listen", listen_detour, FnListen, FN_LISTEN);

    replace!(
        hook_manager,
        "connect",
        connect_detour,
        FnConnect,
        FN_CONNECT
    );

    replace!(hook_manager, "fcntl", fcntl_detour, FnFcntl, FN_FCNTL);
    replace!(hook_manager, "dup", dup_detour, FnDup, FN_DUP);
    replace!(hook_manager, "dup2", dup2_detour, FnDup2, FN_DUP2);

    replace!(
        hook_manager,
        "getpeername",
        getpeername_detour,
        FnGetpeername,
        FN_GETPEERNAME
    );

    replace!(
        hook_manager,
        "getsockname",
        getsockname_detour,
        FnGetsockname,
        FN_GETSOCKNAME
    );

    replace!(
        hook_manager,
        "gethostname",
        gethostname_detour,
        FnGethostname,
        FN_GETHOSTNAME
    );

    #[cfg(target_os = "linux")]
    {
        // Here we replace a function of libuv and not libc, so we pass None as the .
        replace!(
            hook_manager,
            "uv__accept4",
            uv__accept4_detour,
            FnUv__accept4,
            FN_UV__ACCEPT4
        );

        replace!(
            hook_manager,
            "accept4",
            accept4_detour,
            FnAccept4,
            FN_ACCEPT4
        );

        replace!(hook_manager, "dup3", dup3_detour, FnDup3, FN_DUP3);
    }

    replace!(hook_manager, "accept", accept_detour, FnAccept, FN_ACCEPT);

    if enabled_remote_dns {
        replace!(
            hook_manager,
            "getaddrinfo",
            getaddrinfo_detour,
            FnGetaddrinfo,
            FN_GETADDRINFO
        );

        replace!(
            hook_manager,
            "freeaddrinfo",
            freeaddrinfo_detour,
            FnFreeaddrinfo,
            FN_FREEADDRINFO
        );
    }
}
