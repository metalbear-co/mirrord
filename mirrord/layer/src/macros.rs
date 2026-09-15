// macros moved to layer-core
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub(crate) use mirrord_layer_core::hook_symbol;
pub(crate) use mirrord_layer_core::replace;
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub(crate) use mirrord_layer_core::replace_with_fallback;
