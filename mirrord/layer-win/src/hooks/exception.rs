//! Hook that keeps mirrord's unhandled-exception filter installed.
//!
//! A target's runtime (its C runtime, or std) typically calls `SetUnhandledExceptionFilter` during
//! startup, which would replace mirrord's crash filter and leave real crashes uncaptured. The crash
//! handler installs ours first; this hook then makes any later call a no-op for the OS slot. The
//! caller's filter is remembered as the chain target instead, so it still runs after ours.
//!
//! See [`utils_win::diagnostics::crash::adopt_previous_filter`].

use std::sync::OnceLock;

use minhook_detours_rs::guard::DetourGuard;
use mirrord_layer_lib::error::LayerResult;
use winapi::um::errhandlingapi::LPTOP_LEVEL_EXCEPTION_FILTER;

use crate::apply_hook;

/// The `SetUnhandledExceptionFilter` signature.
type SetUnhandledExceptionFilterFn =
    unsafe extern "system" fn(LPTOP_LEVEL_EXCEPTION_FILTER) -> LPTOP_LEVEL_EXCEPTION_FILTER;

static SET_UEF_ORIGINAL: OnceLock<&SetUnhandledExceptionFilterFn> = OnceLock::new();

/// Detour for `SetUnhandledExceptionFilter`.
///
/// It never installs the caller's filter. Mirrord's stays the OS top-level filter. The caller's
/// filter becomes the chain target, and the previous chain target is returned to the caller.
unsafe extern "system" fn set_unhandled_exception_filter_detour(
    filter: LPTOP_LEVEL_EXCEPTION_FILTER,
) -> LPTOP_LEVEL_EXCEPTION_FILTER {
    utils_win::diagnostics::crash::adopt_previous_filter(filter)
}

/// Installs the filter-guard hook.
///
/// # Arguments
///
/// * `guard` - the detour guard the hook is registered with.
pub fn initialize_hooks(guard: &mut DetourGuard<'static>) -> LayerResult<()> {
    apply_hook!(
        guard,
        "kernelbase",
        "SetUnhandledExceptionFilter",
        set_unhandled_exception_filter_detour,
        SetUnhandledExceptionFilterFn,
        SET_UEF_ORIGINAL
    )?;
    Ok(())
}
