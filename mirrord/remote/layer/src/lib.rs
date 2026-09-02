#![cfg(unix)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

use std::cmp::Ordering;

use ctor::ctor;
use libc::pid_t;
use mirrord_layer_core::{hooks::HookManager, replace};
use mirrord_layer_lib::logging::init_tracing;
use mirrord_layer_macro::hook_guard_fn;
use tracing::trace;

use crate::{claimed_sockets::claimed_sockets, hooks::enable_socket_hooks};

mod claimed_sockets;
mod error;
#[cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux"
))]
mod go;
mod handoff;
mod hooks;

#[ctor]
fn mirrord_layer_entry_point() {
    if cfg!(test) {
        return;
    }
    init_tracing();

    trace!("remote-layer initializing hooks");
    let mut hook_manager = HookManager::default();
    unsafe { enable_hooks(&mut hook_manager) };
    trace!("remote-layer hooks installed");
}

unsafe fn enable_hooks(hook_manager: &mut HookManager) {
    unsafe {
        replace!(hook_manager, "fork", fork_detour, FnFork, FN_FORK);
    }

    unsafe { enable_socket_hooks(hook_manager) };

    // The libc hooks above are invisible to a Go application, which issues its syscalls without
    // going through libc.
    #[cfg(all(
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_os = "linux"
    ))]
    crate::go::enable_hooks(hook_manager);
}

/// Hooks `libc::fork` to keep claimed-socket bookkeeping usable in the child.
///
/// A multithreaded process can otherwise fork while another thread holds the
/// claimed-socket mutex, leaving the child with a permanently locked copy after
/// that thread disappears. Locking it in the forking thread also gives the child
/// a consistent snapshot of the claimed sockets; both processes release their
/// copy of the lock after `fork` returns.
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn fork_detour() -> pid_t {
    let claimed_sockets = claimed_sockets()
        .lock()
        .expect("claimed socket lock failed");

    unsafe {
        trace!("remote-layer process forking");
        let res = FN_FORK();

        match res.cmp(&0) {
            Ordering::Equal => trace!("remote-layer child process continuing after fork"),
            Ordering::Greater => trace!("remote-layer child process id is {res}"),
            Ordering::Less => trace!("remote-layer fork failed"),
        }

        drop(claimed_sockets);
        res
    }
}
