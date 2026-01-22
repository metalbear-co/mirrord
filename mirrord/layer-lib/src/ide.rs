//! IDE integration detection utilities.
//!
//! This module provides functions to detect various IDE-related execution modes
//! for the mirrord layer.

/// Environment variable set when running in IDE-orchestrated mode.
/// In this mode, the IDE manages the mirrord environment, so we only need to
/// install process hooks for DLL propagation to child processes without full
/// layer initialization.
///
/// This is set by `mirrord exec --ide-orchestrated` and propagated down the line
/// by being inherited.
pub const MIRRORD_IDE_ORCHESTRATED_ENV: &str = "__MIRRORD_IDE_ORCHESTRATED";

/// Environment variable that will be present only in the user code processes.
///
/// This is set by the IDE extension to be passed to the user code processes.
pub const MIRRORD_EXT_INJECTED_ENV: &str = "__MIRRORD_EXT_INJECTED";

/// Check if running in IDE-orchestrated mode.
///
/// In this mode, the IDE manages the mirrord environment, so we only need to
/// install process hooks for DLL propagation to child processes without full
/// layer initialization.
pub fn is_ide_orchestrated() -> bool {
    std::env::var(MIRRORD_IDE_ORCHESTRATED_ENV)
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
}

/// Check if the extension has injected the layer.
pub fn is_ext_injected() -> bool {
    std::env::var(MIRRORD_EXT_INJECTED_ENV).is_ok()
}

/// Check if this is a user process (the actual target process).
///
/// A user process is one where:
/// - The extension has injected the target environment variables (`__MIRRORD_EXT_INJECTED` is set)
/// - The current executable is NOT the mirrord CLI itself
pub fn is_user_process() -> bool {
    if !is_ext_injected() {
        return false;
    }

    let Ok(current_exe) = std::env::current_exe() else {
        return false;
    };

    let Some(file_name) = current_exe.file_stem() else {
        return false;
    };

    let file_name_lower = file_name.to_string_lossy().to_lowercase();
    file_name_lower != "mirrord"
}
