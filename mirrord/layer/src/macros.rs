// macros moved to layer-core
pub(crate) use mirrord_layer_core::replace;
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub(crate) use mirrord_layer_core::replace_with_fallback;
