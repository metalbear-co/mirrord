#![cfg(unix)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

use ctor::ctor;
use mirrord_layer_core::hooks::HookManager;
use mirrord_layer_lib::logging::init_tracing;

use crate::socket::hooks::enable_socket_hooks;

mod socket;

#[ctor]
fn mirrord_layer_entry_point() {
    if cfg!(test) {
        return;
    }
    init_tracing();

    let mut hook_manager = HookManager::default();
    unsafe { enable_socket_hooks(&mut hook_manager) };
}
