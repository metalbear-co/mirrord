use std::{
    any::Any,
    collections::HashMap,
    ffi::CStr,
    os::unix::io::RawFd,
    sync::{LazyLock, OnceLock},
};

use frida_gum::interceptor::Interceptor;
use libc::{c_char, c_int, sockaddr, socklen_t};
use mirrord_macro::hook_fn;
use mirrord_protocol::AddrInfoHint;
use os_socketaddr::OsSocketAddr;
use tracing::{debug, error, trace, warn};

use super::ops::*;
use crate::{
    error::LayerError,
    hook2,
    macros::{hook, try_hook},
    socket::AddrInfoHintExt,
    GUM,
};

#[hook_fn]
pub(super) unsafe extern "C" fn socket_detour(
    domain: c_int,
    type_: c_int,
    protocol: c_int,
) -> c_int {
    socket(domain, type_, protocol)
}

#[hook_fn]
unsafe extern "C" fn bind_detour(
    sockfd: c_int,
    addr: *const sockaddr,
    addrlen: socklen_t,
) -> c_int {
    bind(sockfd, addr, addrlen)
}

#[hook_fn]
unsafe extern "C" fn listen_detour(sockfd: RawFd, backlog: c_int) -> c_int {
    listen(sockfd, backlog)
}

#[hook_fn]
unsafe extern "C" fn connect_detour(
    sockfd: RawFd,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> c_int {
    let address =
        match OsSocketAddr::from_raw_parts(raw_address as *const _, address_length as usize)
            .into_addr()
            .ok_or(LayerError::AddressConversion)
        {
            Ok(address) => address,
            Err(fail) => return fail.into(),
        };

    let (Ok(result) | Err(result)) =
        connect(sockfd, address)
            .map(|()| 0)
            .map_err(|fail| match fail {
                LayerError::LocalFDNotFound(fd) => libc::connect(fd, raw_address, address_length),
                other => other.into(),
            });

    result
}

#[hook_fn]
unsafe extern "C" fn getpeername_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    getpeername(sockfd, address, address_len)
}

#[hook_fn]
unsafe extern "C" fn getsockname_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> i32 {
    getsockname(sockfd, address, address_len)
}

#[hook_fn]
unsafe extern "C" fn accept_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> i32 {
    let accept_fd = FN_ACCEPT(sockfd, address, address_len);

    if accept_fd == -1 {
        accept_fd
    } else {
        accept(sockfd, address, address_len, accept_fd)
    }
}

#[cfg(target_os = "linux")]
#[hook_fn(alias = "uv__accept4")]
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

/// We have a different version for macOS as a workaround for https://github.com/metalbear-co/mirrord/issues/184
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[hook_fn]
unsafe extern "C" fn fcntl_detour(fd: c_int, cmd: c_int, mut arg: ...) -> c_int {
    let arg = arg.arg::<usize>();
    let fcntl_fd = libc::fcntl(fd, cmd, arg);
    fcntl(fd, cmd, fcntl_fd)
}

#[cfg(not(all(target_arch = "aarch64", target_os = "macos")))]
#[hook_fn]
unsafe extern "C" fn fcntl_detour(fd: c_int, cmd: c_int, arg: ...) -> c_int {
    let fcntl_fd = libc::fcntl(fd, cmd, arg);
    fcntl(fd, cmd, fcntl_fd)
}

#[hook_fn]
unsafe extern "C" fn dup_detour(fd: c_int) -> c_int {
    let dup_fd = libc::dup(fd);
    dup(fd, dup_fd)
}

#[hook_fn]
unsafe extern "C" fn dup2_detour(oldfd: c_int, newfd: c_int) -> c_int {
    if oldfd == newfd {
        return newfd;
    }
    let dup2_fd = libc::dup2(oldfd, newfd);
    dup(oldfd, dup2_fd)
}

#[cfg(target_os = "linux")]
#[hook_fn]
unsafe extern "C" fn dup3_detour(oldfd: c_int, newfd: c_int, flags: c_int) -> c_int {
    let dup3_fd = libc::dup3(oldfd, newfd, flags);
    dup(oldfd, dup3_fd)
}

/// Turns the raw pointer parameters into Rust types and calls `ops::getaddrinfo`.
///
/// # Warning:
/// - `raw_hostname`, `raw_servname`, and/or `raw_hints` might be null!
#[hook_fn]
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

    getaddrinfo(node, service, hints)
        .map(|c_addr_info_ptr| {
            out_addr_info.copy_from_nonoverlapping(&c_addr_info_ptr, 1);

            0
        })
        .map_err(|fail| {
            error!("Failed resolving DNS with {:#?}", fail);

            match fail {
                LayerError::IO(io_fail) => io_fail.raw_os_error().unwrap(),
                LayerError::DNSNoName => libc::EAI_NONAME,
                _ => libc::EAI_FAIL,
            }
        })
        .unwrap_or_else(|fail| fail)
}

/// Deallocates a `*mut libc::addrinfo` that was previously allocated with `Box::new` in
/// `getaddrinfo_detour` and converted into a raw pointer by `Box::into_raw`.
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
#[hook_fn]
unsafe extern "C" fn freeaddrinfo_detour(addrinfo: *mut libc::addrinfo) {
    trace!("freeaddrinfo_detour -> addrinfo {:#?}", *addrinfo);

    // Iterate over `addrinfo` linked list dropping it.
    let mut current = addrinfo;
    while !current.is_null() {
        let current_box = Box::from_raw(current);

        current = (*current).ai_next;
        drop(current_box);
    }
}

// static FN_SOCKET2: LazyLock<HashMap<String, Box<dyn Any + 'static>>> = LazyLock::new(|| {
//     let mut vtable = HashMap::with_capacity(32);
//     let mut interceptor = frida_gum::interceptor::Interceptor::obtain(&GUM);

//     unsafe {
//         hook2!(&mut interceptor, "socket", socket_detour, FnSocket)
//             .and_then(|h| Ok(vtable.insert("socket".to_string(), h)))
//     };

//     vtable
// });

static FN_SOCKET2: LazyLock<FnSocket> = LazyLock::new(|| {
    let intercept = |interceptor: &mut frida_gum::interceptor::Interceptor,
                     symbol_name,
                     detour: FnSocket|
     -> Result<FnSocket, LayerError> {
        let function = frida_gum::Module::find_export_by_name(None, symbol_name)
            .ok_or(LayerError::NoExportName(symbol_name.to_string()))?;

        let replaced = interceptor.replace(
            function,
            frida_gum::NativePointer(detour as *mut libc::c_void),
            frida_gum::NativePointer(std::ptr::null_mut()),
        )?;

        let original_fn: FnSocket = unsafe { std::mem::transmute(replaced) };

        Ok(original_fn)
    };

    let mut interceptor = frida_gum::interceptor::Interceptor::obtain(&GUM);
    intercept(&mut interceptor, "socket", socket_detour).unwrap_or(libc::socket)
});

pub(crate) unsafe fn enable_socket_hooks(interceptor: &mut Interceptor, enabled_remote_dns: bool) {
    /*
        let _ = hook2!(interceptor, "socket", socket_detour, FnSocket)
            .and_then(|h| Ok(FN_SOCKET.set(h).unwrap()));

        let _ =
            hook2!(interceptor, "bind", bind_detour, FnBind).and_then(|h| Ok(FN_BIND.set(h).unwrap()));

        let _ = hook2!(interceptor, "listen", listen_detour, FnListen)
            .and_then(|h| Ok(FN_LISTEN.set(h).unwrap()));

        let _ = hook2!(interceptor, "connect", connect_detour, FnConnect)
            .and_then(|h| Ok(FN_CONNECT.set(h).unwrap()));

        let _ = hook2!(interceptor, "fcntl", fcntl_detour, FnFcntl)
            .and_then(|h| Ok(FN_FCNTL.set(h).unwrap()));

        let _ = hook2!(interceptor, "dup", dup_detour, FnDup).and_then(|h| Ok(FN_DUP.set(h).unwrap()));

        let _ =
            hook2!(interceptor, "dup2", dup2_detour, FnDup2).and_then(|h| Ok(FN_DUP2.set(h).unwrap()));

        let _ = hook2!(
            interceptor,
            "getpeername",
            getpeername_detour,
            FnGetpeername
        )
        .and_then(|h| Ok(FN_GETPEERNAME.set(h).unwrap()));

        let _ = hook2!(
            interceptor,
            "getsockname",
            getsockname_detour,
            FnGetsockname
        )
        .and_then(|h| Ok(FN_GETSOCKNAME.set(h).unwrap()));

        #[cfg(target_os = "linux")]
        {
            // TODO(alex) [high] 2022-07-25: Most of these are pretty much the same thing, with a few
            // exceptions:
            //
            // 1. detour function has a different name: `uv__accept4` to `accept4`;
            // 2. enabled by flags;
            //
            // To solve (1), the macro just has to take an argument `detour`, so it'll try to use the
            // normal function name, and the argument version as well. Has to be both for cases where
            // we have many different functions pointing to the same fn.
            //
            // Solving (2) requires some sort of `Option` + `bool` deal. As things should be enabled by
            // default, meaning lack of `enabled =` flags is the same as `enabled = true`. But we don't
            // have access to the true value in `hook_fn`, so it has to be a check in the middle of
            // initialization. I'm starting to think that maybe we could initialize either to the
            // detour or to the `libc` version, this way we cover both cases (enabled/disabled), these
            // being the left and right sides of this either variant.
            let _ = hook2!(interceptor, "uv__accept4", accept4_detour, FnAccept4)
                .or_else(|fail| {
                    warn!(
                        "enable_socket_hooks -> Failed hooking `uv__accept4` with {:#?}!",
                        fail
                    );

                    hook2!(interceptor, "accept4", accept4_detour, FnAccept4)
                })
                .and_then(|h| Ok(FN_ACCEPT4.set(h).unwrap()));

            let _ = hook2!(interceptor, "dup3", dup3_detour, FnDup3)
                .and_then(|h| Ok(FN_DUP3.set(h).unwrap()));
        }

        let _ = hook2!(interceptor, "accept", accept_detour, FnAccept)
            .and_then(|h| Ok(FN_ACCEPT.set(h).unwrap()));

        if enabled_remote_dns {
            let _ = hook2!(
                interceptor,
                "getaddrinfo",
                getaddrinfo_detour,
                FnGetaddrinfo
            )
            .and_then(|h| Ok(FN_GETADDRINFO.set(h).unwrap()));

            let _ = hook2!(
                interceptor,
                "freeaddrinfo",
                freeaddrinfo_detour,
                FnFreeaddrinfo
            )
            .and_then(|h| Ok(FN_FREEADDRINFO.set(h).unwrap()));
        }
    */
}

/*
   let intercept = |interceptor: &mut frida_gum::interceptor::Interceptor,
                    symbol_name,
                    detour: FnSocket|
    -> Result<FnSocket, LayerError> {
       let function = frida_gum::Module::find_export_by_name(None, symbol_name)
           .ok_or(LayerError::NoExportName(symbol_name.to_string()))?;

       let replaced = interceptor.replace(
           function,
           frida_gum::NativePointer(detour as *mut libc::c_void),
           frida_gum::NativePointer(std::ptr::null_mut()),
       )?;

       let original_fn: FnSocket = unsafe { std::mem::transmute(replaced) };

       Ok(original_fn)
   };

   let mut interceptor = frida_gum::interceptor::Interceptor::obtain(&GUM);
   intercept(&mut interceptor, "socket", socket_detour)

*/
