#![feature(once_cell)]
#![warn(clippy::indexing_slicing)]

use std::{
    io::stdout,
    sync::{
        atomic::{AtomicUsize, Ordering},
        OnceLock,
    },
    time::Duration,
};

use serde::Serialize;
use serde_json::to_string;
use termspin::{spinner::dots, Group, Line, Loop, SharedFrames};

/// The environment variable name that is used
/// to determine the mode of progress reporting
pub const MIRRORD_PROGRESS_ENV: &str = "MIRRORD_PROGRESS_MODE";

/// `ProgressMode` specifies the way progress is reported
/// by [`TaskProgress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    /// Display dynamic progress with spinners.
    Standard,
    /// Display simple human-readable messages in new lines.
    Simple,
    /// Output progress messages in JSON format for programmatic use.
    Json,
    /// Do not output progress.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Success,
    Failure,
    Warning,
}

/// Initialize progress reporting based on the [`MIRRORD_PROGRESS_ENV`]
/// environment variable.
///
/// If the variable does not exist, the provided fallback
/// progress mode is used.
///
/// This is automatically called with the default
/// progress mode whenever a [`TaskProgress`] is created.
pub fn init_from_env(fallback: ProgressMode) {
    PROGRESS.get_or_init(|| ProgressReport::from_env(fallback));
}

static PROGRESS: OnceLock<ProgressReport> = OnceLock::new();

fn get_report() -> &'static ProgressReport {
    PROGRESS.get_or_init(|| {
        ProgressReport::from_env(if atty::is(atty::Stream::Stdout) {
            ProgressMode::Standard
        } else {
            ProgressMode::Simple
        })
    })
}

/// Global progress state.
struct ProgressReport {
    spinner_loop: Loop<SharedFrames<Group>>,
    mode: ProgressMode,
}

impl ProgressReport {
    fn new(mode: ProgressMode) -> Self {
        Self {
            mode,
            spinner_loop: Loop::new(Duration::from_millis(100), Group::new().shared()),
        }
    }

    fn from_env(fallback: ProgressMode) -> Self {
        Self::new(match std::env::var(MIRRORD_PROGRESS_ENV).as_deref() {
            Ok("std" | "standard") => ProgressMode::Standard,
            Ok("dumb" | "simple") => ProgressMode::Simple,
            Ok("json") => ProgressMode::Json,
            Ok("off") => ProgressMode::Off,
            _ => fallback,
        })
    }
}

static TASK_COUNT: AtomicUsize = AtomicUsize::new(0);

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

pub trait Progress: Sized {
    /// Create a subtask report from this task.
    fn subtask(&self, text: &str) -> Self;

    fn done(mut self) {
        self.set_done(None, false);
    }

    /// Finish the task with the given message.
    fn done_with(mut self, text: &str) {
        self.set_done(Some(text), false);
    }

    /// Fail the task without changing the description.
    fn fail(mut self) {
        self.set_done(None, true);
    }

    /// Fail the task with the given message.
    fn fail_with(mut self, text: &str) {
        self.set_done(Some(text), true);
    }

    fn set_done(&mut self, msg: Option<&str>, failure: bool);
    fn print_message(&self, kind: MessageKind, msg: Option<&str>);
}

pub struct NoProgress;

impl Progress for NoProgress {
    fn subtask(&self, _: &str) -> NoProgress {
        NoProgress
    }

    fn set_done(&mut self, _: Option<&str>, _: bool) {}
    fn print_message(&self, _: MessageKind, _: Option<&str>) {}
}

/// Progress report for a single task.
pub struct TaskProgress {
    group: SharedFrames<Group>,
    line: SharedFrames<Line>,
    indent: usize,
    done: bool,
    fail_on_drop: bool,
    name: String,
}

impl TaskProgress {
    /// Report the progress of a new task with the given description.
    ///
    /// By default the task is considered failed if it goes out of
    /// scope without `done` being called.
    pub fn new(text: &str) -> Self {
        let report = get_report();

        let mut group = Group::new();
        let line = Line::new(dots()).with_text(text).shared();

        group.push(line.clone());

        let group = group.shared();

        match report.mode {
            ProgressMode::Standard => {
                report.spinner_loop.inner().lock().push(group.clone());
                report.spinner_loop.spawn_stream(stdout());
            }
            ProgressMode::Simple => {
                println!("{text}");
            }
            ProgressMode::Json => {
                let message = ProgressMessage::NewTask(NewTaskMessage {
                    name: text.to_string(),
                    ..Default::default()
                });
                message.print();
            }
            ProgressMode::Off => {}
        }

        TASK_COUNT.fetch_add(1, Ordering::Relaxed);

        Self {
            group,
            line,
            indent: 0,
            done: false,
            fail_on_drop: true,
            name: text.to_string(),
        }
    }

    /// If set to `true` (default), the task is considered failed
    /// if it gets dropped.
    ///
    /// Set this to `false` if the task cannot fail, or failure
    /// is not important.
    pub fn fail_on_drop(mut self, fail: bool) -> Self {
        self.fail_on_drop = fail;
        self
    }
}

impl Progress for TaskProgress {
    /// Create a subtask report from this task.
    fn subtask(&self, text: &str) -> TaskProgress {
        let report = get_report();
        let sub_indent = self.indent + 1;

        let mut group = Group::new().with_indent(sub_indent);
        let line = Line::new(dots()).with_text(text).shared();

        group.push(line.clone());

        let group = group.shared();

        self.group.lock().push(group.clone());

        match report.mode {
            ProgressMode::Standard | ProgressMode::Off => {}
            ProgressMode::Simple => {
                println!("{text}");
            }
            ProgressMode::Json => {
                let message = ProgressMessage::NewTask(NewTaskMessage {
                    name: text.to_string(),
                    parent: Some(self.name.clone()),
                });
                message.print();
            }
        }

        TASK_COUNT.fetch_add(1, Ordering::Relaxed);

        TaskProgress {
            group,
            line,
            indent: sub_indent,
            done: false,
            fail_on_drop: true,
            name: text.to_string(),
        }
    }

    fn set_done(&mut self, msg: Option<&str>, failure: bool) {
        self.done = true;
        let kind = if failure {
            MessageKind::Failure
        } else {
            MessageKind::Success
        };
        self.print_message(kind, msg);
    }
    fn print_message(&self, kind: MessageKind, msg: Option<&str>) {
        let report = get_report();

        match report.mode {
            ProgressMode::Standard => {
                let mut line = self.line.lock();

                line.set_spinner_visible(false);

                let marker = match kind {
                    MessageKind::Success => '✓',
                    MessageKind::Failure => 'x',
                    MessageKind::Warning => '!',
                };

                if let Some(msg) = msg {
                    line.set_text(&format!("{marker} {msg}"));
                } else {
                    let msg = format!("{marker} {}", line.text());
                    line.set_text(&msg);
                }
            }
            ProgressMode::Simple => {
                if let Some(msg) = msg {
                    println!("{msg}");
                }
            }
            ProgressMode::Json => {
                let message = if kind == MessageKind::Warning {
                    ProgressMessage::Warning(WarningMessage {
                        message: msg.unwrap().to_string(),
                    })
                } else {
                    ProgressMessage::FinishedTask(FinishedTaskMessage {
                        name: self.name.clone(),
                        success: kind == MessageKind::Success,
                        message: msg.map(|s| s.to_string()),
                    })
                };
                message.print();
            }
            ProgressMode::Off => {}
        }
    }
}

impl Drop for TaskProgress {
    fn drop(&mut self) {
        let count = TASK_COUNT.fetch_sub(1, Ordering::Relaxed);

        if !self.done {
            self.set_done(None, self.fail_on_drop);
        }

        if count == 1 {
            let report = get_report();
            report.spinner_loop.stop();

            report.spinner_loop.inner().lock().retain(|_| false);
        }
    }
}
