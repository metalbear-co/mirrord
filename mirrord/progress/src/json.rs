//! The messages `JsonProgress` prints, one JSON object per line, which the IDE extensions parse.
//!
//! They live apart from the renderers so that [`crate::finished_task_line`] can print a finish in
//! the same format without them: the layer finishes `mirrord exec`'s running task from inside the
//! program, which has no use for the terminal renderers.

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize, Debug, Clone, Default)]
pub(crate) struct NewTaskMessage {
    /// Task name (identifier)
    pub(crate) name: String,
    /// Parent task name, if subtask.
    pub(crate) parent: Option<String>,
}

/// Message sent when a task is finished.
#[derive(Serialize, Debug, Clone, Default)]
pub(crate) struct FinishedTaskMessage {
    /// Finished task name
    name: String,
    /// Was the task successful?
    success: bool,
    /// Finish message
    message: Option<String>,
}

/// Message sent when a task is finished.
#[derive(Serialize, Debug, Clone, Default)]
pub(crate) struct WarningMessage {
    /// Warning message
    pub(crate) message: String,
}

/// The message types that we report on [`Progress`](crate::Progress).
///
/// These are used by the extensions (vscode and intellij) to show nice notifications.
#[derive(Serialize, Debug, Clone)]
#[serde(tag = "type")]
pub(crate) enum ProgressMessage {
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
        /// Should be an `IdeMessage` converted to [`Value`].
        message: Value,
    },
}

impl ProgressMessage {
    /// The finish of the task `name`, the one way every finish is built.
    pub(crate) fn finished_task(name: &str, success: bool, message: Option<&str>) -> Self {
        Self::FinishedTask(FinishedTaskMessage {
            name: name.to_owned(),
            success,
            message: message.map(str::to_owned),
        })
    }

    /// The message as the JSON line the IDEs parse, without its newline.
    pub(crate) fn to_line(&self) -> String {
        // Strings, booleans and a JSON value, with no map keys that aren't strings, always
        // serialize.
        serde_json::to_string(self).expect("a progress message always serializes to JSON")
    }
}
