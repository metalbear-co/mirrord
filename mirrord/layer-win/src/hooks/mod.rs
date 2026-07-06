//! Module responsible for providing [`initialize_hooks`].

pub(crate) mod exception;
pub(crate) mod files;
pub(crate) mod macros;
pub(crate) mod process;
pub(crate) mod socket;

use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::{
    error::{LayerError, LayerResult},
    setup::setup,
};

pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> LayerResult<()> {
    let setup = setup();

    // Eagerly spawn the shared background thread pool from this safe layer
    // thread, so a later `task_pool::submit` from inside a hook (async file
    // read, async DNS) never has to spawn a thread under the loader lock.
    crate::task_pool::initialize();

    // Initialize IOCP module prerequisites. Pre-step: must run before
    // any FS hook is initialized so the async-read worker can post
    // completion packets.
    crate::iocp::initialize()?;

    // Always enable process hooks (required for Windows DLL injection)
    if setup.process_hooks_enabled() {
        tracing::info!("Enabling process hooks (always required on Windows)");
        process::initialize_hooks(guard)?;
    }

    // Keep mirrord's crash filter from being overridden by the target's runtime.
    exception::initialize_hooks(guard)?;

    // NOTE(gabriela): currently I believe the ideal way to handle this is
    // through hook-level checks
    tracing::info!(
        "Enabling file system hooks (flag:{})",
        setup.fs_hooks_enabled()
    );
    files::initialize_hooks(guard)?;

    // Conditionally enable socket hooks
    if setup.socket_hooks_enabled() || setup.dns_hooks_enabled() {
        tracing::info!(
            "Enabling socket hooks (socket: {}, dns: {})",
            setup.socket_hooks_enabled(),
            setup.dns_hooks_enabled()
        );
        socket::initialize_hooks(guard, setup)?;
    } else {
        tracing::info!("Socket hooks disabled by configuration (no network features enabled)");
    }

    guard
        .enable_all_hooks()
        .map_err(|err| LayerError::DetourGuard(err.to_string()))?;
    Ok(())
}
