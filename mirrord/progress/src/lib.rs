#![deny(unused_crate_dependencies)]

#[cfg(feature = "implementations")]
pub mod implementations;
pub mod messages;

#[cfg(feature = "implementations")]
pub use implementations::*;

/// The environment variable name that is used
/// to determine the mode of progress reporting
pub const MIRRORD_PROGRESS_ENV: &str = "MIRRORD_PROGRESS_MODE";

/// Progress report API for displaying notifications in cli/extensions.
///
/// This is our IDE friendly way of sending notification messages from the cli, be careful not to
/// mix it (e.g. calling `progress.info`) with regular [`println!`], as the IDE may fail to parse
/// the output (intellij will fail with `"failed to parse a message from mirrord
/// binary"`), and we end up displaying an error instead.
#[cfg_attr(feature = "implementations", enum_dispatch::enum_dispatch)]
pub trait Progress: Sized + Send + Sync {
    /// Create a subtask report from this task.
    fn subtask(&self, text: &str) -> Self;

    /// When task is done successfully
    fn success(&mut self, _: Option<&str>) {}

    /// When task is done with failure
    fn failure(&mut self, _: Option<&str>) {}

    /// When you want to issue a warning on current task
    fn warning(&self, _: &str) {}

    /// When you want to print a message, IDE support.
    fn info(&self, _: &str) {}

    /// When you want to send a message to the IDE that doesn't need to be shown to the user.
    ///
    /// You may use this to pass additional context to the IDE through the `value` object.
    fn ide(&self, _: serde_json::Value) {}

    /// When you want to print a message, cli only.
    fn print(&self, _: &str) {}

    /// When you want to print a message, cli only, after progress has succeeded.
    ///
    /// Only for `SpinnerProgress`.
    fn add_to_print_buffer(&mut self, _: &str) {}

    /// Control if drop without calling success is considered failure.
    fn set_fail_on_drop(&mut self, _: bool) {}

    /// Hide the progress task temporarily to execute a function without stdout interference. This
    /// is particularly useful when interactive cluster auth mode is used.
    ///
    /// Only for `SpinnerProgress`.
    fn suspend<F: FnOnce() -> R, R>(&self, f: F) -> R {
        f()
    }
}

#[derive(Debug)]
pub struct NullProgress;

impl Progress for NullProgress {
    fn subtask(&self, _: &str) -> NullProgress {
        NullProgress
    }
}

/// [`ProgressTracker`] determines the way progress is reported.
#[cfg(feature = "implementations")]
#[derive(Debug)]
#[enum_dispatch::enum_dispatch(Progress)]
pub enum ProgressTracker {
    /// Display dynamic progress with spinners.
    SpinnerProgress(SpinnerProgress),

    /// Display simple human-readable messages in new lines.
    SimpleProgress(SimpleProgress),

    /// Output progress messages in JSON format for programmatic use.
    JsonProgress(JsonProgress),

    /// Do not output progress.
    NullProgress(NullProgress),
}

#[cfg(feature = "implementations")]
impl ProgressTracker {
    /// Get the progress tracker from environment or return a default ([`SpinnerProgress`]).
    ///
    /// Automatically appends current version to the title.
    pub fn from_env(title: &str) -> Self {
        Self::try_from_env(title).unwrap_or_else(|| {
            let title = format!("{title} ({})", env!("CARGO_PKG_VERSION"));
            SpinnerProgress::new(&title).into()
        })
    }

    /// Get the progress tracker from environment.
    ///
    /// Automatically appends current version to the title.
    pub fn try_from_env(title: &str) -> Option<Self> {
        let title_with_version = format!("{title} ({})", env!("CARGO_PKG_VERSION"));
        let progress = match std::env::var(MIRRORD_PROGRESS_ENV).as_deref() {
            Ok("dumb" | "simple") => SimpleProgress::new(&title_with_version).into(),
            // adding version in title breaks IDE extension.
            Ok("json") => JsonProgress::new(title).into(),
            Ok("off") => NullProgress.into(),
            Ok("std" | "standard") => SpinnerProgress::new(&title_with_version).into(),
            _ => return None,
        };

        Some(progress)
    }
}
