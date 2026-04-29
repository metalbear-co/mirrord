//! We implement each hook function in a safe function as much as possible, having the unsafe do the
//! absolute minimum
use std::net::SocketAddr;

use libc::{sockaddr, socklen_t};
pub use mirrord_layer_lib::{
    detour::{Bypass, Detour},
    error::HookError,
    socket::{SHARED_SOCKETS_ENV_VAR, SOCKETS, UserSocket},
};
use socket2::SockAddr;

pub(super) mod hooks;
pub(crate) mod ops;

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
