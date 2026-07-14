#![cfg(unix)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

use ctor::ctor;
use mirrord_layer_core::hooks::HookManager;
use mirrord_layer_lib::logging::init_tracing;
use tracing::trace;

use crate::socket::hooks::enable_socket_hooks;

mod error;
mod socket;

#[ctor]
fn mirrord_layer_entry_point() {
    if cfg!(test) {
        return;
    }
    init_tracing();

    trace!("remote-layer initializing socket hooks");
    let mut hook_manager = HookManager::default();
    unsafe { enable_socket_hooks(&mut hook_manager) };
    trace!("remote-layer socket hooks installed");
}
