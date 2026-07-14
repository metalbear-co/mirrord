use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};

use libc::{c_int, sockaddr, socklen_t};
use mirrord_layer_core::{hooks::HookManager, replace};
use mirrord_layer_lib::detour::{Bypass, Detour};
use mirrord_layer_macro::hook_guard_fn;
use mirrord_remote_layer_protocol::ConnectionHandoffVerdict;

use super::connection_handoff::handoff_remote_connection;
use crate::socket::ops::{
    claim_placeholder_socket, claimed_socket, fill_address, remove_claimed_socket,
    socket_addr_from_fd, socket_peer_addr_from_fd,
};

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn close_detour(fd: c_int) -> c_int {
    remove_claimed_socket(fd);

    unsafe { FN_CLOSE(fd) }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn getsockname_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    if let Some(claimed_socket) = claimed_socket(sockfd) {
        return fill_address(address, address_len, claimed_socket.local_address.into())
            .unwrap_or_bypass_with(|_| unsafe { FN_GETSOCKNAME(sockfd, address, address_len) });
    }

    unsafe { FN_GETSOCKNAME(sockfd, address, address_len) }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn getpeername_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    if let Some(claimed_socket) = claimed_socket(sockfd) {
        return fill_address(address, address_len, claimed_socket.peer_address.into())
            .unwrap_or_bypass_with(|_| unsafe { FN_GETPEERNAME(sockfd, address, address_len) });
    }

    unsafe { FN_GETPEERNAME(sockfd, address, address_len) }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn accept_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    unsafe {
        let accepted_fd = FN_ACCEPT(sockfd, address, address_len);
        if accepted_fd == -1 {
            return accepted_fd;
        }

        let accepted_fd = OwnedFd::from_raw_fd(accepted_fd);
        accept(sockfd, &accepted_fd).unwrap_or_bypass_with(|_| accepted_fd.into_raw_fd())
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
    unsafe {
        let accepted_fd = FN_ACCEPT4(sockfd, address, address_len, flags);
        if accepted_fd == -1 {
            tracing::trace!(sockfd, flags, "remote-layer accept4 hook returned -1");
            return accepted_fd;
        }

        let accepted_fd = OwnedFd::from_raw_fd(accepted_fd);
        accept(sockfd, &accepted_fd).unwrap_or_bypass_with(|_| accepted_fd.into_raw_fd())
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
    unsafe { accept4_detour(sockfd, address, address_len, flags) }
}

#[cfg(target_os = "macos")]
#[hook_guard_fn]
pub(super) unsafe extern "C" fn accept_nocancel_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    unsafe {
        let accepted_fd = FN_ACCEPT_NOCANCEL(sockfd, address, address_len);
        if accepted_fd == -1 {
            return accepted_fd;
        }

        let accepted_fd = OwnedFd::from_raw_fd(accepted_fd);
        accept(sockfd, &accepted_fd).unwrap_or_bypass_with(|_| accepted_fd.into_raw_fd())
    }
}

/// Runs the remote incoming handoff for an accepted socket.
///
/// The syscall hook converts the raw fd returned by `accept*` into an `OwnedFd`
/// before calling this helper, so ownership stays explicit while we decide
/// whether to hand the connection off to the agent or fall back locally.
///
/// If this helper returns [`Detour::Success`], the hook wrapper should return
/// the fd carried by the `Success` variant. In the claim path, that is a
/// placeholder socket fd created by [`claim_placeholder_socket`], not the
/// original accepted fd.
///
/// If it returns [`Detour::Bypass`], the hook wrapper must turn the `OwnedFd`
/// back into the original raw fd with `into_raw_fd()` so the caller keeps the
/// socket it accepted.
#[mirrord_layer_macro::instrument(level = tracing::Level::TRACE)]
fn accept(sockfd: c_int, accepted_fd: &OwnedFd) -> Detour<c_int> {
    let listener_address = match socket_addr_from_fd(sockfd) {
        Ok(address) => address,
        Err(error) => {
            tracing::warn!(%error, sockfd, "failed to read accepted listener address");
            return Detour::Bypass(Bypass::RemoteIncomingFallback);
        }
    };

    let local_address = match socket_addr_from_fd(accepted_fd.as_raw_fd()) {
        Ok(address) => address,
        Err(error) => {
            tracing::warn!(%error, sockfd, "failed to read accepted local address");
            return Detour::Bypass(Bypass::RemoteIncomingFallback);
        }
    };

    let peer_address = match socket_peer_addr_from_fd(accepted_fd.as_raw_fd()) {
        Ok(address) => address,
        Err(error) => {
            tracing::warn!(%error, sockfd, "failed to read accepted peer address");
            return Detour::Bypass(Bypass::RemoteIncomingFallback);
        }
    };

    match handoff_remote_connection(
        listener_address,
        local_address,
        peer_address,
        accepted_fd.as_raw_fd(),
    ) {
        Ok(response) => match response.verdict {
            ConnectionHandoffVerdict::Rejected => {
                tracing::debug!(
                    sockfd,
                    accepted_fd = accepted_fd.as_raw_fd(),
                    "remote-layer connection handoff declined"
                );
                Detour::Bypass(Bypass::RemoteIncomingFallback)
            }
            ConnectionHandoffVerdict::Accepted {
                placeholder_address,
            } => match claim_placeholder_socket(
                accepted_fd,
                placeholder_address,
                local_address,
                peer_address,
            ) {
                Ok(placeholder_fd) => {
                    tracing::debug!(
                        sockfd,
                        placeholder_fd = placeholder_fd.as_raw_fd(),
                        "remote-layer connection handoff accepted with placeholder socket"
                    );
                    Detour::Success(placeholder_fd.into_raw_fd())
                }
                Err(error) => {
                    tracing::warn!(%error, sockfd, "failed to create placeholder socket");
                    Detour::Bypass(Bypass::RemoteIncomingFallback)
                }
            },
        },
        Err(error) => {
            tracing::warn!(%error, sockfd, "connection handoff for accepted fd failed");
            Detour::Bypass(Bypass::RemoteIncomingFallback)
        }
    }
}

pub(crate) unsafe fn enable_socket_hooks(hook_manager: &mut HookManager) {
    unsafe {
        replace!(hook_manager, "close", close_detour, FnClose, FN_CLOSE);
        replace!(
            hook_manager,
            "getsockname",
            getsockname_detour,
            FnGetsockname,
            FN_GETSOCKNAME
        );
        replace!(
            hook_manager,
            "getpeername",
            getpeername_detour,
            FnGetpeername,
            FN_GETPEERNAME
        );
        replace!(hook_manager, "accept", accept_detour, FnAccept, FN_ACCEPT);

        #[cfg(target_os = "linux")]
        {
            replace!(
                hook_manager,
                "accept4",
                accept4_detour,
                FnAccept4,
                FN_ACCEPT4
            );
            replace!(
                hook_manager,
                "uv__accept4",
                uv__accept4_detour,
                FnUv__accept4,
                FN_UV__ACCEPT4
            );
        }

        #[cfg(target_os = "macos")]
        {
            replace!(
                hook_manager,
                "accept$NOCANCEL",
                accept_nocancel_detour,
                FnAccept_nocancel,
                FN_ACCEPT_NOCANCEL
            );
        }
    }
}
