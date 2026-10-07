//! Module responsible for providing [`initialize_hooks`].

pub(crate) mod exception;
pub(crate) mod files;
// Re-exported from `utils-win`, where the crash handler can reach it too. This path is what
// the `internal_bypass` macro expands to, so it must keep this name.
pub(crate) use utils_win::internal_thread;
pub(crate) mod macros;
pub(crate) mod process;
pub(crate) mod socket;

#[cfg(test)]
mod bypass_contract;

use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::{
    error::{LayerError, LayerResult},
    setup::setup,
};

/// Creates every hook family, then enables them all at once. Runs inside `DllMain`.
///
/// Every `apply_hook!` only creates a disabled hook, and `enable_all_hooks` patches all of them in
/// one transaction that either applies completely or not at all. So an error anywhere here leaves
/// no function patched, and the caller can report the failure with the target untouched.
///
/// mirrord's crash filter goes into the OS slot just before the hooks are enabled, because once
/// the `SetUnhandledExceptionFilter` hook is live, no call through the public API reaches that
/// slot any more (see [`exception`]). It is taken back out if enabling fails.
pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> LayerResult<()> {
    let setup = setup();

    // Initialize IOCP module prerequisites. Pre-step: must run before
    // any FS hook is initialized so the async-read worker can post
    // completion packets.
    crate::iocp::initialize()?;

    // Always enable process hooks (required for Windows DLL injection)
    if setup.process_hooks_enabled() {
        tracing::info!("Enabling process hooks (always required on Windows)");
        process::initialize_hooks(guard)?;
    }

    // Keep mirrord's crash filter from being overridden by the target's runtime. Extension-managed
    // runs do not install that filter, so the target must retain normal ownership of this API.
    let crash_reporting = utils_win::diagnostics::crash_reporting_enabled();
    if crash_reporting {
        exception::initialize_hooks(guard)?;
    }

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

    if crash_reporting {
        utils_win::diagnostics::crash::install_filter();
    }

    guard.enable_all_hooks().map_err(|err| {
        utils_win::diagnostics::crash::restore_filter();
        LayerError::DetourGuard(err.to_string())
    })
}
