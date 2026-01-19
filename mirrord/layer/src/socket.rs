//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::{
    collections::HashMap,
    net::SocketAddr,
    os::unix::io::RawFd,
    sync::{Arc, LazyLock, Mutex},
};

use base64::prelude::*;
use libc::{sockaddr, socklen_t};
pub use mirrord_layer_lib::{
    detour::{Bypass, Detour},
    socket::UserSocket,
};
use mirrord_protocol::outgoing::SocketAddress;
use socket2::SockAddr;
use tracing::warn;

use crate::{
    common,
    error::{HookError, HookResult},
};

pub(super) mod hooks;
pub(crate) mod ops;

pub(crate) const SHARED_SOCKETS_ENV_VAR: &str = "MIRRORD_SHARED_SOCKETS";

/// Stores the [`UserSocket`]s created by the user.
///
/// **Warning**: Do not put logs in here! If you try logging stuff inside this initialization
/// you're gonna have a bad time. The process hanging is the min you should expect, if you
/// choose to ignore this warning.
///
/// - [`SHARED_SOCKETS_ENV_VAR`]: Some sockets may have been initialized by a parent process through
///   [`libc::execve`] (or any `exec*`), and the spawned children may want to use those sockets. As
///   memory is not shared via `exec*` calls (unlike `fork`), we need a way to pass parent sockets
///   to child processes. The way we achieve this is by setting the [`SHARED_SOCKETS_ENV_VAR`] with
///   an [`BASE64_URL_SAFE`] encoded version of our [`SOCKETS`]. The env var is set as
///   `MIRRORD_SHARED_SOCKETS=({fd}, {UserSocket}),*`.
///
/// - [`libc::FD_CLOEXEC`] behaviour: While rebuilding sockets from the env var, we also check if
///   they're set with the cloexec flag, so that children processes don't end up using sockets that
///   are exclusive for their parents.
pub(crate) static SOCKETS: LazyLock<Mutex<HashMap<RawFd, Arc<UserSocket>>>> = LazyLock::new(|| {
    std::env::var(SHARED_SOCKETS_ENV_VAR)
        .ok()
        .and_then(|encoded| {
            BASE64_URL_SAFE
                .decode(encoded.into_bytes())
                .inspect_err(|error| {
                    tracing::warn!(
                        ?error,
                        "failed decoding base64 value from {SHARED_SOCKETS_ENV_VAR}"
                    )
                })
                .ok()
        })
        .and_then(|decoded| {
            bincode::decode_from_slice::<Vec<(i32, UserSocket)>, _>(
                &decoded,
                bincode::config::standard(),
            )
            .inspect_err(|error| tracing::warn!(?error, "failed parsing shared sockets env value"))
            .ok()
        })
        .map(|(fds_and_sockets, _)| {
            Mutex::new(HashMap::from_iter(fds_and_sockets.into_iter().filter_map(
                |(fd, socket)| {
                    // Do not inherit sockets that are `FD_CLOEXEC`.
                    // NOTE: The original `fcntl` is called instead of `FN_FCNTL` because the latter
                    // may be null at this point, likely due to child-spawning functions that mess
                    // with memory such as fork/exec.
                    // See: https://github.com/metalbear-co/mirrord-intellij/issues/374
                    if unsafe { libc::fcntl(fd, libc::F_GETFD, 0) != -1 } {
                        Some((fd, Arc::new(socket)))
                    } else {
                        None
                    }
                },
            )))
        })
        .unwrap_or_default()
});

#[inline]
fn is_ignored_port(addr: &SocketAddr) -> bool {
    addr.port() == 0
}

/// Fill in the sockaddr structure for the given address.
#[inline]
fn fill_address(
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_address: SockAddr,
) -> Detour<i32> {
    let result = if address.is_null() {
        Ok(0)
    } else if address_len.is_null() {
        Err(HookError::NullPointer)
    } else {
        unsafe {
            let len = std::cmp::min(*address_len as usize, new_address.len() as usize);

            std::ptr::copy_nonoverlapping(
                new_address.as_ptr() as *const u8,
                address as *mut u8,
                len,
            );
            *address_len = new_address.len();
        }

        Ok(0)
    }?;

    Detour::Success(result)
}
