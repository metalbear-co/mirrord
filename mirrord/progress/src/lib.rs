use std::{collections::HashSet, time::Duration};

use enum_dispatch::enum_dispatch;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::Serialize;
use serde_json::{to_string, Value};

pub mod messages;

/// The environment variable name that is used
/// to determine the mode of progress reporting
pub const MIRRORD_PROGRESS_ENV: &str = "MIRRORD_PROGRESS_MODE";

/// Progress report API for displaying notifications in cli/extensions.
///
/// This is our IDE friendly way of sending notification messages from the cli, be careful not to
/// mix it (e.g. calling `progress.info`) with regular [`println!`], as the IDE may fail to parse
/// the [`ProgressMessage`] (intellij will fail with `"failed to parse a message from mirrord
/// binary"`), and we end up displaying an error instead.
#[enum_dispatch]
pub trait Progress: Sized {
    /// Create a subtask report from this task.
    fn subtask(&self, text: &str) -> Self;

    /// When task is done successfully
    fn success(&mut self, msg: Option<&str>);

    /// When task is done with failure
    fn failure(&mut self, msg: Option<&str>);

    /// When you want to issue a warning on current task
    fn warning(&self, msg: &str);

    /// When you want to print a message, IDE support.
    fn info(&self, msg: &str);

    /// When you want to send a message to the IDE that doesn't need to be shown to the user.
    ///
    /// You may use this to pass additional context to the IDE through the `value` object.
    fn ide(&self, value: serde_json::Value);

    /// When you want to print a message, cli only.
    fn print(&self, msg: &str);

    /// Control if drop without calling succes is considered failure.
    fn set_fail_on_drop(&mut self, fail: bool);
}

/// `ProgressMode` specifies the way progress is reported
/// by [`TaskProgress`].
#[derive(Debug)]
#[enum_dispatch(Progress)]
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

#[derive(Debug)]
pub struct NullProgress;

impl Progress for NullProgress {
    fn subtask(&self, _: &str) -> NullProgress {
        NullProgress
    }

    fn set_fail_on_drop(&mut self, _: bool) {}

    fn success(&mut self, _: Option<&str>) {}

    fn failure(&mut self, _: Option<&str>) {}

    fn warning(&self, _: &str) {}

    fn info(&self, _: &str) {}

    fn ide(&self, _: serde_json::Value) {}

    fn print(&self, _: &str) {}
}

#[derive(Debug)]
pub struct JsonProgress {
    parent: Option<String>,
    name: String,
    done: bool,
    fail_on_drop: bool,
}

impl JsonProgress {
    pub fn new(text: &str) -> JsonProgress {
        let progress = JsonProgress {
            parent: None,
            name: text.to_string(),
            done: false,
            fail_on_drop: true,
        };
        progress.print_new_task();
        progress
    }

    fn print_new_task(&self) {
        let message = ProgressMessage::NewTask(NewTaskMessage {
            name: self.name.clone(),
            parent: self.parent.clone(),
        });
        message.print();
    }

    fn print_finished_task(&self, success: bool, msg: Option<&str>) {
        let message = ProgressMessage::FinishedTask(FinishedTaskMessage {
            name: self.name.clone(),
            message: msg.map(|s| s.to_string()),
            success,
        });
        message.print();
    }
}

impl Progress for JsonProgress {
    fn subtask(&self, text: &str) -> JsonProgress {
        let task = JsonProgress {
            parent: Some(self.name.clone()),
            name: text.to_string(),
            done: false,
            fail_on_drop: true,
        };
        task.print_new_task();
        task
    }

    fn print(&self, _: &str) {}

    fn info(&self, msg: &str) {
        let message = ProgressMessage::Info {
            message: msg.to_string(),
        };
        message.print();
    }

    fn ide(&self, value: serde_json::Value) {
        if std::env::var("MIRRORD_PROGRESS_SUPPORT_IDE")
            .ok()
            .and_then(|ide_support_enabled| ide_support_enabled.parse().ok())
            .unwrap_or(false)
        {
            let message = ProgressMessage::IdeMessage { message: value };
            message.print();
        }
    }

    fn warning(&self, msg: &str) {
        let message = ProgressMessage::Warning(WarningMessage {
            message: msg.to_string(),
        });
        message.print();
    }

    fn failure(&mut self, msg: Option<&str>) {
        self.done = true;
        self.print_finished_task(false, msg)
    }

    fn success(&mut self, msg: Option<&str>) {
        self.done = true;
        self.print_finished_task(true, msg)
    }

    fn set_fail_on_drop(&mut self, fail: bool) {
        self.fail_on_drop = fail;
    }
}

impl Drop for JsonProgress {
    fn drop(&mut self) {
        if !self.done {
            if self.fail_on_drop {
                self.failure(None);
            } else {
                self.success(None);
            }
        }
    }
}

#[derive(Debug)]
pub struct SimpleProgress;

impl SimpleProgress {
    fn new(text: &str) -> SimpleProgress {
        println!("{text}");
        SimpleProgress {}
    }
}

impl Progress for SimpleProgress {
    fn subtask(&self, text: &str) -> SimpleProgress {
        println!("{text}");
        SimpleProgress {}
    }

    fn print(&self, text: &str) {
        println!("{text}");
    }

    fn warning(&self, msg: &str) {
        println!("{msg}");
    }

    fn info(&self, msg: &str) {
        println!("{msg}");
    }

    fn ide(&self, _: serde_json::Value) {}

    fn failure(&mut self, msg: Option<&str>) {
        println!("{msg:?}");
    }

    fn success(&mut self, msg: Option<&str>) {
        println!("{msg:?}");
    }

    fn set_fail_on_drop(&mut self, _: bool) {}
}

fn spinner_template(indent: usize) -> String {
    format!("{indent}{{spinner}} {{msg}}", indent = "  ".repeat(indent))
}

fn spinner(indent: usize) -> ProgressBar {
    ProgressBar::hidden().with_style(
        ProgressStyle::default_spinner()
            .template(&spinner_template(indent))
            .unwrap(),
    )
}

#[derive(Debug)]
pub struct SpinnerProgress {
    done: bool,
    fail_on_drop: bool,
    root_progress: MultiProgress,
    progress: ProgressBar,
    indent: usize,
}

impl SpinnerProgress {
    fn new(text: &str) -> SpinnerProgress {
        let root_progress = MultiProgress::new();
        let progress = spinner(0);
        progress.set_message(text.to_string());
        root_progress.add(progress.clone());
        progress.enable_steady_tick(Duration::from_millis(60));

        SpinnerProgress {
            done: false,
            fail_on_drop: true,
            indent: 0,
            root_progress,
            progress,
        }
    }
}

impl Progress for SpinnerProgress {
    fn subtask(&self, text: &str) -> SpinnerProgress {
        let indent = self.indent + 1;
        let progress = spinner(indent);
        progress.set_message(text.to_string());
        self.root_progress.add(progress.clone());
        progress.enable_steady_tick(Duration::from_millis(60));
        SpinnerProgress {
            done: false,
            fail_on_drop: true,
            root_progress: self.root_progress.clone(),
            indent,
            progress,
        }
    }

    fn print(&self, msg: &str) {
        let _ = self.root_progress.println(msg);
    }

    fn warning(&self, msg: &str) {
        let formatted_message = format!("! {msg}");
        self.print(&formatted_message);
        self.progress.set_message(formatted_message);
    }

    fn info(&self, msg: &str) {
        let formatted_message = format!("* {msg}");
        self.print(&formatted_message);
        self.progress.set_message(formatted_message);
    }

    fn ide(&self, _: serde_json::Value) {}

    fn failure(&mut self, msg: Option<&str>) {
        self.done = true;
        if let Some(msg) = msg {
            self.progress.abandon_with_message(format!("x {msg}"));
        } else {
            self.progress
                .abandon_with_message(format!("x {}", self.progress.message()));
        }
    }

    fn success(&mut self, msg: Option<&str>) {
        self.done = true;
        if let Some(msg) = msg {
            self.progress.finish_with_message(format!("✓ {msg}"));
        } else {
            self.progress
                .finish_with_message(format!("✓ {}", self.progress.message()));
        }
    }

    fn set_fail_on_drop(&mut self, fail: bool) {
        self.fail_on_drop = fail;
    }
}

impl Drop for SpinnerProgress {
    fn drop(&mut self) {
        if !self.done {
            if self.fail_on_drop {
                self.failure(None);
            } else {
                self.success(None);
            }
        }
    }
}

impl ProgressTracker {
    /// Get the progress tracker from environment or return a default ([`SpinnerProgress`]).
    pub fn from_env(text: &str) -> Self {
        Self::try_from_env(text).unwrap_or_else(|| SpinnerProgress::new(text).into())
    }

    /// Get the progress tracker from environment.
    pub fn try_from_env(text: &str) -> Option<Self> {
        let progress = match std::env::var(MIRRORD_PROGRESS_ENV).as_deref() {
            Ok("dumb" | "simple") => SimpleProgress::new(text).into(),
            Ok("json") => JsonProgress::new(text).into(),
            Ok("off") => NullProgress.into(),
            Ok("std" | "standard") => SpinnerProgress::new(text).into(),
            _ => return None,
        };

        Some(progress)
    }
}

/// Message sent when a new task is created using subtask/new
#[derive(Serialize, Debug, Clone, Default)]
struct NewTaskMessage {
    /// Task name (indentifier)
    name: String,
    /// Parent task name, if subtask.
    parent: Option<String>,
}

/// Message sent when a task is finished.
#[derive(Serialize, Debug, Clone, Default)]
struct FinishedTaskMessage {
    /// Finished task name
    name: String,
    /// Was the task successful?
    success: bool,
    /// Finish message
    message: Option<String>,
}

/// Message sent when a task is finished.
#[derive(Serialize, Debug, Clone, Default)]
struct WarningMessage {
    /// Warning message
    message: String,
}

/// Indicates what type of notification should appear in the IDEs.
#[derive(Serialize, Debug, Clone, Default)]
pub enum NotificationLevel {
    /// Normal info box.
    #[default]
    Info,

    /// Warning box.
    Warning,
}

/// Action/button type that appears in the pop-up notifications.
#[derive(Serialize, Debug, Clone, PartialEq, Eq, Hash)]
#[serde(tag = "kind")]
pub enum IdeAction {
    /// A link action, where `label` is the text, and `link` is the _href_.
    Link { label: String, link: String },
}

/// Messages sent to the IDEs with full context.
#[derive(Serialize, Debug, Clone, Default)]
pub struct IdeMessage {
    /// Allows us to identify this message and map it to something meaningful in the IDEs.
    ///
    /// Not shown to the user.
    ///
    /// In vscode, this should map to a `configEntry` defined in `package.json`.
    pub id: String,

    /// The level of the notification, the type of pop-up it'll be displayed in the IDEs.
    pub level: NotificationLevel,

    /// Message content.
    pub text: String,

    /// Actions/buttons that appears in the pop-up notification.
    pub actions: HashSet<IdeAction>,
}

/// The message types that we report on [`Progress`].
///
/// These are used by the extensions (vscode and intellij) to show nice notifications.
#[derive(Serialize, Debug, Clone)]
#[serde(tag = "type")]
enum ProgressMessage {
    NewTask(NewTaskMessage),
    Warning(WarningMessage),
    FinishedTask(FinishedTaskMessage),
    Info {
        message: String,
    },
    /// Messages that are passed to the IDE and shown to the user in notification boxes.
    IdeMessage {
        /// It's a generic json [`Value`].
        ///
        /// Should be an [`IdeMessage`] converted to [`Value`].
        message: Value,
    },
}

impl ProgressMessage {
    pub(crate) fn print(&self) {
        println!("{}", to_string(self).unwrap());
    }
}
