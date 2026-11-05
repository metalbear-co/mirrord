#![deny(unused_crate_dependencies)]

#[cfg(feature = "implementations")]
pub mod implementations;
#[cfg_attr(
    not(feature = "implementations"),
    allow(
        dead_code,
        reason = "without the renderers, only a finished task is ever printed"
    )
)]
mod json;
pub mod messages;

#[cfg(feature = "implementations")]
pub use implementations::*;

use crate::json::ProgressMessage;

/// The environment variable name that is used
/// to determine the mode of progress reporting
pub const MIRRORD_PROGRESS_ENV: &str = "MIRRORD_PROGRESS_MODE";

/// A progress mode that prints one line per event, the modes machines read: `simple` for
/// `mirrord up`, `json` for the IDEs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineMode {
    /// `SimpleProgress`'s plain lines.
    Simple,
    /// `JsonProgress`'s JSON lines.
    Json,
}

impl LineMode {
    /// The mode's name, as [`MIRRORD_PROGRESS_ENV`] spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Simple => "simple",
            Self::Json => "json",
        }
    }

    /// The mode named `name`, as [`Self::as_str`] spells it; anything else is `None`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "simple" => Some(Self::Simple),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// The line that finishing `task` successfully with `message` prints in `mode`, newline included.
///
/// Byte for byte what `SimpleProgress` and `JsonProgress` print for `success(Some(message))`, for
/// code that finishes a task without them: the layer finishes `mirrord exec`'s running task with
/// `Ready!` from inside the program. `simple` prints the message alone, as `SimpleProgress` does.
pub fn finished_task_line(mode: LineMode, task: &str, message: &str) -> String {
    match mode {
        LineMode::Simple => format!("{message}\n"),
        LineMode::Json => format!(
            "{}\n",
            ProgressMessage::finished_task(task, true, Some(message)).to_line()
        ),
    }
}

/// The environment variable name used to communicate which IDE is running the CLI.
///
/// Expected values: `"vscode"`, `"intellij"`.
/// Falls back to `"plugin"` when unset.
pub const MIRRORD_IDE_NAME_ENV: &str = "MIRRORD_IDE_NAME";

/// Returns the UTM medium to use in links, based on [`MIRRORD_IDE_NAME_ENV`].
pub fn utm_medium() -> String {
    std::env::var(MIRRORD_IDE_NAME_ENV).unwrap_or_else(|_| "plugin".to_owned())
}

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

    /// Another tracker, with a prefix on every task name and message.
    PrefixedProgress(PrefixedProgress),
}

#[cfg(feature = "implementations")]
impl ProgressTracker {
    /// Get the progress tracker from environment or return a default ([`SpinnerProgress`]).
    ///
    /// Automatically appends current version to the title.
    pub fn from_env(title: &str) -> Self {
        Self::try_from_env(title).unwrap_or_else(|| {
            let title = format!("{title} (cli version {})", env!("CARGO_PKG_VERSION"));
            SpinnerProgress::new(&title).into()
        })
    }

    /// Get the progress tracker from environment.
    ///
    /// Automatically appends current version to the title.
    pub fn try_from_env(title: &str) -> Option<Self> {
        let title_with_version = format!("{title} (cli version {})", env!("CARGO_PKG_VERSION"));
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

    /// The one-line-per-event mode this tracker prints in, or `None` for a tracker meant for a
    /// person (the spinner, a prefixed tracker) or one that prints nothing.
    pub fn line_mode(&self) -> Option<LineMode> {
        match self {
            Self::SimpleProgress(_) => Some(LineMode::Simple),
            Self::JsonProgress(_) => Some(LineMode::Json),
            Self::SpinnerProgress(_) | Self::NullProgress(_) | Self::PrefixedProgress(_) => None,
        }
    }

    /// Returns a tracker with `NullProgress`.
    pub fn null() -> Self {
        Self::NullProgress(NullProgress)
    }

    /// Wraps this tracker so that every task created from it, and every message reported on
    /// them, starts with `"<prefix>: "` (see [`PrefixedProgress`]).
    pub fn prefixed(self, prefix: &str) -> Self {
        PrefixedProgress::new(prefix.to_owned(), self).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{EXEC_RUNNING_TASK, SESSION_READY_MESSAGE};

    /// `mirrord up` matches the plain line `SimpleProgress` prints for `success(Some("Ready!"))`.
    #[test]
    fn a_simple_ready_is_the_message_alone() {
        assert_eq!(
            finished_task_line(LineMode::Simple, EXEC_RUNNING_TASK, SESSION_READY_MESSAGE),
            "Ready!\n",
            "mirrord up matches this exact line"
        );
    }

    /// The IDEs match the running task's finish by this exact line, the one `JsonProgress` prints
    /// for `success(Some("Ready!"))` on the `running process` task.
    #[test]
    fn a_json_ready_is_the_line_json_progress_prints() {
        assert_eq!(
            finished_task_line(LineMode::Json, EXEC_RUNNING_TASK, SESSION_READY_MESSAGE),
            "{\"type\":\"FinishedTask\",\"name\":\"running process\",\"success\":true,\"message\":\"Ready!\"}\n",
            "the IDEs parse this exact line"
        );
    }
}
