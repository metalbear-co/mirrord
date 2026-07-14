//! Module responsible for providing [`initialize_hooks`].

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

    tracing::info!(
        event = "windows_layer_init",
        stage = "hook_phases_begin",
        process_hooks = setup.process_hooks_enabled(),
        fs_hooks = setup.fs_hooks_enabled(),
        socket_hooks = setup.socket_hooks_enabled(),
        dns_hooks = setup.dns_hooks_enabled(),
        "starting Windows hook initialization phases"
    );

    // Eagerly spawn the shared background thread pool from this safe layer
    // thread, so a later `task_pool::submit` from inside a hook (async file
    // read, async DNS) never has to spawn a thread under the loader lock.
    crate::task_pool::initialize();
    tracing::info!(
        event = "windows_layer_init",
        stage = "task_pool_ready",
        "initialized Windows layer task pool"
    );

    // Initialize IOCP module prerequisites. Pre-step: must run before
    // any FS hook is initialized so the async-read worker can post
    // completion packets.
    crate::iocp::initialize()?;
    tracing::info!(
        event = "windows_layer_init",
        stage = "iocp_ready",
        "initialized Windows IO completion support"
    );

    // Always enable process hooks (required for Windows DLL injection)
    if setup.process_hooks_enabled() {
        tracing::info!("Enabling process hooks (always required on Windows)");
        process::initialize_hooks(guard)?;
        tracing::info!(
            event = "windows_layer_init",
            stage = "process_hooks_ready",
            "initialized Windows process hooks"
        );
    }

    // NOTE(gabriela): currently I believe the ideal way to handle this is
    // through hook-level checks
    tracing::info!(
        "Enabling file system hooks (flag:{})",
        setup.fs_hooks_enabled()
    );
    files::initialize_hooks(guard)?;
    tracing::info!(
        event = "windows_layer_init",
        stage = "file_hooks_ready",
        "initialized Windows file hooks"
    );

    // Conditionally enable socket hooks
    if setup.socket_hooks_enabled() || setup.dns_hooks_enabled() {
        tracing::info!(
            "Enabling socket hooks (socket: {}, dns: {})",
            setup.socket_hooks_enabled(),
            setup.dns_hooks_enabled()
        );
        socket::initialize_hooks(guard, setup)?;
        tracing::info!(
            event = "windows_layer_init",
            stage = "socket_hooks_ready",
            "initialized Windows socket hooks"
        );
    } else {
        tracing::info!("Socket hooks disabled by configuration (no network features enabled)");
    }

    tracing::info!(
        event = "windows_layer_init",
        stage = "enable_all_hooks_begin",
        "enabling all initialized Windows hooks"
    );
    guard
        .enable_all_hooks()
        .map_err(|err| LayerError::DetourGuard(err.to_string()))?;
    tracing::info!(
        event = "windows_layer_init",
        stage = "enable_all_hooks_complete",
        "enabled all initialized Windows hooks"
    );
    Ok(())
}
