//! Module responsible for providing [`initialize_hooks`].

pub(crate) mod exception;
pub(crate) mod files;
// Re-exported from `utils-win`, where the crash handler can reach it too. This path is what
// the `internal_bypass` macro expands to, so it must keep this name.
pub(crate) use utils_win::internal_thread;
pub(crate) mod macros;
pub(crate) mod process;
pub(crate) mod socket;

use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::{
    error::{LayerError, LayerResult},
    setup::setup,
};
use winapi::um::errhandlingapi::{GetLastError, SetLastError};

/// Runs `log` so that it cannot change what the caller of a detour observes.
///
/// It holds two obligations.
///
/// A panic must not leave the detour. A detour is an `extern "system"` function that Windows calls,
/// and Rust ends the process rather than let a panic cross that boundary. The panic this catches is
/// the residual kind: logging on a thread whose storage is gone is already stopped in the
/// subscriber's filter, which covers every call site (see `layer-lib`'s `logging`). On MSVC that is
/// a fast-fail (`0xC0000409`) which no handler can catch and no dump can record. Logging reaches
/// this on its own: `tracing` reads a thread-local for every event, and reading one while the
/// thread is being torn down panics with `AccessError`.
///
/// The thread's last error must read the same after the log line as before it. Logging calls Win32
/// and writes a file, and each of those sets it. A caller reads that value after the real API
/// returns, so the log line must not be what it finds.
///
/// Use this for any logging that happens after a detour has called the original function. Do not
/// use it where the detour means to report an error of its own.
///
/// # Arguments
///
/// * `log` - the logging to attempt. It must do nothing the target depends on.
pub(crate) fn log_without_disturbing_caller(log: impl FnOnce()) {
    let last_error = unsafe { GetLastError() };

    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(log));

    unsafe { SetLastError(last_error) };
}

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

    // Keep mirrord's crash filter from being overridden by the target's runtime. Extension-managed
    // runs do not install that filter, so the target must retain normal ownership of this API.
    if utils_win::diagnostics::crash_reporting_enabled() {
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

    guard
        .enable_all_hooks()
        .map_err(|err| LayerError::DetourGuard(err.to_string()))?;
    Ok(())
}
