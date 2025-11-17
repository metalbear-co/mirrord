//! Module responsible for exposing macros to create sycall hooks.

/// Macro to apply hook.
///
/// # Arguments
///
/// * `guard` - Instance, or mutable reference of [`minhook_detours_rs::guard::DetourGuard`].
/// * `dll` - Name of the target API's DLL.
/// * `fn` - Name of the target API.
/// * `detour` - Rust detour for target API.
/// * `detour_type` - Type of target API detour.
/// * `original` - Variable holding an [`OnceLock`] to an underlying `&detour_type`.
///
/// # Returns
///
/// - `Ok(())` if the operation succeeded.
/// - `Err(Error::FailedApplyingAPIHook)` if the operation failed.
#[macro_export]
macro_rules! apply_hook {
    ($guard:ident, $dll:literal, $fn:literal, $detour:ident, $detour_type:ty, $original:ident) => {
        $original
            .set(
                $guard
                    .create_hook::<$detour_type>(
                        $crate::process::get_export($dll, $fn) as _,
                        $detour as _,
                    )
                    .map_err(
                        |err| mirrord_layer_lib::error::LayerError::HookEngineApply {
                            function: $fn,
                            dll: $dll,
                            error: err.to_string(),
                        },
                    )?,
            )
            .or(Err(
                mirrord_layer_lib::error::LayerError::FailedApplyingAPIHook(
                    $fn.into(),
                    $dll.into(),
                ),
            ))
    };
}
