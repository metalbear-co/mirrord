//! Windows crash and exception diagnostics.
//!
//! This module is the shared home for the crash machinery. The injected layer installs the
//! in-process handler. The CLI runs the out-of-process monitor. Both draw from here.
//!
//! Crash reporting is on by default and can be disabled for extension-managed sessions. A
//! full-memory dump is a separate opt-in via [`full_memory_dump`].

use std::{path::PathBuf, sync::OnceLock};

use chrono::Local;
use mirrord_config::{MIRRORD_LAYER_CRASH_REPORTING, MIRRORD_LAYER_FULL_MEMORY_DUMP};

pub mod crash;
pub mod dialog;
pub mod dump;
pub(crate) mod handle;
pub mod monitor;
pub mod report;

/// The env var naming the layer-log directory. Crash artifacts are grouped there when it is set.
///
/// This mirrors `mirrord_layer_lib::logging::MIRRORD_LAYER_LOG_PATH`; utils-win cannot depend on
/// layer-lib (it would be a dependency cycle), so the name is repeated here.
const MIRRORD_LAYER_LOG_PATH: &str = "MIRRORD_LAYER_LOG_PATH";

/// Whether the layer should install its crash handler and exception-filter hook.
///
/// IDE extensions disable these because their normal stop action terminates the application
/// abruptly. The out-of-process monitor cannot reliably distinguish that lifecycle event from an
/// unexpected external kill.
pub fn crash_reporting_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();

    *ENABLED.get_or_init(|| {
        crash_reporting_enabled_for(std::env::var(MIRRORD_LAYER_CRASH_REPORTING).ok().as_deref())
    })
}

fn crash_reporting_enabled_for(value: Option<&str>) -> bool {
    value.is_none_or(|value| {
        !value.is_empty() && !value.eq_ignore_ascii_case("false") && value != "0"
    })
}

/// Resolves the directory crash artifacts are written to.
///
/// Crash output is grouped with the layer logs when `MIRRORD_LAYER_LOG_PATH` is set, so one place
/// holds both. When it is unset, a `mirrord` subdirectory of the system temp directory is used so
/// artifacts are always written and always findable. Both the layer ([`crash::install`]) and the
/// CLI monitor call this, so the two sides always agree on the location.
pub fn crash_dir() -> PathBuf {
    std::env::var_os(MIRRORD_LAYER_LOG_PATH)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("mirrord"))
}

/// Returns a compact timestamp used in crash artifact file names. For example `20260615_142233`.
pub(crate) fn timestamp() -> String {
    Local::now().format("%Y%m%d_%H%M%S").to_string()
}

/// Replaces every non-`[A-Za-z0-9_-]` character with an underscore.
///
/// Used to make a process name safe for an artifact file name.
pub(crate) fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// Whether to capture a full-memory dump instead of the default bounded minidump.
///
/// When crash reporting is enabled, this selects a full-memory dump instead of the default bounded
/// dump. Full-memory dumps are large and can hold far more secrets, so they require a truthy
/// `MIRRORD_LAYER_FULL_MEMORY_DUMP` value (anything but unset/empty/`false`/`0`).
///
/// # Returns
///
/// `true` when full-memory dumps are requested.
pub fn full_memory_dump() -> bool {
    std::env::var(MIRRORD_LAYER_FULL_MEMORY_DUMP).is_ok_and(|value| {
        !value.is_empty() && !value.eq_ignore_ascii_case("false") && value != "0"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_reporting_defaults_to_enabled() {
        assert!(crash_reporting_enabled_for(None));
    }

    #[test]
    fn crash_reporting_can_be_disabled() {
        assert!(!crash_reporting_enabled_for(Some("false")));
        assert!(!crash_reporting_enabled_for(Some("FALSE")));
        assert!(!crash_reporting_enabled_for(Some("0")));
        assert!(!crash_reporting_enabled_for(Some("")));
        assert!(crash_reporting_enabled_for(Some("true")));
        assert!(crash_reporting_enabled_for(Some("1")));
    }

    #[test]
    fn sanitize_name_replaces_unsafe_chars() {
        assert_eq!(sanitize_name("python3.12"), "python3_12");
        assert_eq!(sanitize_name("my-app_v2"), "my-app_v2");
        assert_eq!(sanitize_name("a b/c\\d"), "a_b_c_d");
        assert_eq!(sanitize_name("a@b#c.exe"), "a_b_c_exe");
    }
}
