use mirrord_layer_lib::ide::{is_ext_injected, is_ide_orchestrated, is_user_process};

/// Check if we're in IDE-only mode (process hooks only).
///
/// This mode is active when:
/// - We're in IDE-orchestrated mode (the IDE is managing mirrord)
/// - We're NOT in the user process (the actual target application)
///
/// In this mode, we only install process hooks for DLL propagation
/// without full layer initialization - the IDE manages the mirrord environment.
pub fn is_ide_only_mode() -> bool {
    is_ide_orchestrated() && !is_user_process()
}

/// Check if we're propagating the layer without mirrord environment.
///
/// This is true when IDE-orchestrated mode is active but the extension
/// hasn't injected its environment variables yet.
pub fn is_propagating_layer() -> bool {
    is_ide_orchestrated() && !is_ext_injected()
}
