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
/// * `original` - Variable holding an [`std::sync::OnceLock`] to an underlying `&detour_type`.
///
/// # Returns
///
/// - `Ok(())` if the operation succeeded.
/// - `Err(Error::FailedApplyingAPIHook)` if the operation failed.
#[macro_export]
macro_rules! apply_hook {
    ($guard:ident, $dll:literal, $fn:literal, $detour:ident, $detour_type:ty, $original:ident) => {
        {
            let export = $crate::process::get_export($dll, $fn);
            tracing::trace!(
                event = "windows_layer_hook",
                stage = "export_resolved",
                dll = $dll,
                function = $fn,
                address = ?export,
                "resolved Windows hook export"
            );
            let original = $guard
                .create_hook::<$detour_type>(export as _, $detour as _)
                .map_err(
                    |err| mirrord_layer_lib::error::LayerError::HookEngineApply {
                        function: $fn,
                        dll: $dll,
                        error: err.to_string(),
                    },
                )?;
            $original.set(original).or(Err(
                mirrord_layer_lib::error::LayerError::FailedApplyingAPIHook(
                    $fn.into(),
                    $dll.into(),
                ),
            ))?;
            tracing::trace!(
                event = "windows_layer_hook",
                stage = "created",
                dll = $dll,
                function = $fn,
                "created Windows hook"
            );
            Ok::<(), mirrord_layer_lib::error::LayerError>(())
        }
    };
}
