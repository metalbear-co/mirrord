use enum_dispatch::enum_dispatch;
use indicatif::{ProgressBar, MultiProgress};
use serde::Serialize;
use serde_json::to_string;

/// The environment variable name that is used
/// to determine the mode of progress reporting
pub const MIRRORD_PROGRESS_ENV: &str = "MIRRORD_PROGRESS_MODE";

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
    // /// Display dynamic progress with spinners.
    SpinnerProgress(SpinnerProgress),
    /// Display simple human-readable messages in new lines.
    SimpleProgress(SimpleProgress),
    // /// Output progress messages in JSON format for programmatic use.
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
    fn new(text: &str) -> JsonProgress {
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
    fn failure(&mut self, msg: Option<&str>) {
        println!("{msg:?}");
    }

    fn success(&mut self, msg: Option<&str>) {
        println!("{msg:?}");
    }

    fn set_fail_on_drop(&mut self, _: bool) {}
}


#[derive(Debug)]
pub struct SpinnerProgress {
    done: bool,
    fail_on_drop: bool,
    root_progress: MultiProgress,
    progress: ProgressBar,
}

impl SpinnerProgress {
    fn new(text: &str) -> SpinnerProgress {
        let root_progress = MultiProgress::new();
        let progress = ProgressBar::new_spinner();
        progress.set_message(format!("{text}"));
        root_progress.add(progress.clone());

    
        SpinnerProgress {
            done: false,
            fail_on_drop: true,
            root_progress,
            progress
        }
    }

}

impl Progress for SpinnerProgress {
    fn subtask(&self, text: &str) -> SpinnerProgress {
        let progress = ProgressBar::new_spinner();
        self.root_progress.add(progress.clone());
        progress.set_message(format!("{text}"));
        SpinnerProgress {
            done: false,
            fail_on_drop: true,
            root_progress: self.root_progress.clone(),
            progress
        }
    }

    fn print(&self, _: &str) {}

    fn warning(&self, msg: &str) {
        self.progress.set_message(format!("! {msg}"));
    }

    fn failure(&mut self, msg: Option<&str>) {
        self.done = true;
        if let Some(msg) = msg {
            self.progress.abandon_with_message(format!("{msg}"));
        } else {
            self.progress.set_prefix("x");
            self.progress.abandon();
        }
    }

    fn success(&mut self, msg: Option<&str>) {
        self.done = true;
        self.progress.set_prefix("✓");
        if let Some(msg) = msg {
            self.progress.finish_with_message(format!("{msg}"));
        } else {
            self.progress.finish();
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
    pub fn from_env(fallback: ProgressTracker, text: &str) -> Self {
        match std::env::var(MIRRORD_PROGRESS_ENV).as_deref() {
            Ok("std" | "standard") => SpinnerProgress::new(text).into(),
            Ok("dumb" | "simple") => SimpleProgress::new(text).into(),
            Ok("json") => JsonProgress::new(text).into(),
            Ok("off") => NullProgress.into(),
            _ => fallback,
        }
    }
}

/// Message sent when a new task is created using subtask/new
#[derive(Serialize, Debug, Clone, Default)]
struct NewTaskMessage {
    /// Task name (identifier)
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

#[derive(Serialize, Debug, Clone)]
#[serde(tag = "type")]
enum ProgressMessage {
    NewTask(NewTaskMessage),
    Warning(WarningMessage),
    FinishedTask(FinishedTaskMessage),
}

impl ProgressMessage {
    pub(crate) fn print(&self) {
        println!("{}", to_string(self).unwrap());
    }
}
