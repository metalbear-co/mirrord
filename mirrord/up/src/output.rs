//! What `mirrord up` says about its run, as lines for a terminal or as JSON events for an IDE.
//!
//! Every line `up` prints about a run is an [`Event`], and [`Printer`] writes it in the
//! [`OutputFormat`] `--output` picked. [`Event::text`] is the line `up` prints without the flag,
//! on the stream it goes to; the JSON form is the event itself, always on stdout.
//!
//! The JSON form is one object per line, with `type` saying which event it is. `service` names the
//! up service. Fields are only ever added, so an IDE ignores the ones it doesn't know.
//!
//! | `type` | Fields | When |
//! |---|---|---|
//! | `output` | `service`, `source` (`pre_command`, `session`), `stream` (`stdout`, `stderr`), `line` | a service's process wrote a line |
//! | `progress` | `service`, `progress` | a session's `json` progress message, the object as `mirrord exec` printed it |
//! | `ready` | `service` | the session reported `Ready!`: its program is running |
//! | `pre_command` | `service`, `stage` (`started`, `succeeded`, `failed`, `spawn_failed`), `status`, `code`, `error` | `run.pre_command`'s progress |
//! | `debug_plan` | `service`, `plan` | `debug-plan`'s object, as `mirrord exec` printed it |
//! | `debug_hold` | `service`, `hold` | `debug-hold`'s object: a process or a runtime is held |
//! | `debug_hold_ended` | `service`, `ended` | `debug-hold-ended`'s object |
//! | `debug_note` | `service`, `message` | a runtime connection was refused, or a runtime runs on unheld |
//! | `debug_no_runtime_paused` | `service`, `service_state` (`running`, `exited`) | a runtime was to pause, and none has |
//! | `exited` | `service`, `reason` (`succeeded`, `crashed`, `failed_to_start`, `while_debugged`), `status`, `code`, `error` | a session ended, which ends the run; the service's lines can still follow, until `finished` |
//! | `output_error` | `service`, `stream`, `error` | a service's output couldn't be read |
//! | `note` | `service` (or `null`), `message` | something `up` says before the services start |
//! | `stop_failed` | `error` | a service couldn't be stopped |
//! | `finished` | `error` | always the last line, which ends the stream: the run is over, and `error` is its error, or `null` |
//!
//! Each `error` that isn't a string is a miette JSON report: `message`, `help`, `causes` and the
//! rest, as `mirrord ls` prints its error.
//!
//! On `exited`, it's the session's own error, reported by its `mirrord exec` (see
//! [`SESSION_OUTPUT_ENV`]).

#[cfg(test)]
use std::sync::{Arc, Mutex};
use std::{io, process::ExitStatus};

use miette::{Diagnostic, JSONReportHandler};
use serde::Serialize;
use serde_json::{
    Map, Value,
    value::{RawValue, to_raw_value},
};

/// How `mirrord up` reports its run, picked with `--output`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputFormat {
    /// Lines for people: every service's output behind its name, and the final error as miette
    /// draws it, on stderr.
    #[default]
    Text,
    /// One JSON [`Event`] per line, all on stdout, for IDEs.
    Json,
}

/// Set to [`SESSION_OUTPUT_JSON`] on the sessions of `mirrord up --output json`, and on no
/// others: their `mirrord exec` or `mirrord container` then reports its final error as miette JSON,
/// which `up` puts on the service's `exited` event.
pub const SESSION_OUTPUT_ENV: &str = "MIRRORD_UP_OUTPUT";

/// The value of [`SESSION_OUTPUT_ENV`] that asks for JSON errors.
pub const SESSION_OUTPUT_JSON: &str = "json";

impl OutputFormat {
    /// The format of the `mirrord up` that started this process, a session, from
    /// [`SESSION_OUTPUT_ENV`]. `Text` for a process no `up --output json` started.
    pub fn of_session() -> Self {
        match std::env::var(SESSION_OUTPUT_ENV).as_deref() {
            Ok(SESSION_OUTPUT_JSON) => Self::Json,
            _ => Self::Text,
        }
    }
}

/// Which of a process's streams a line came from, or which of `up`'s a text line goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Stream {
    Stdout,
    Stderr,
}

/// Which of a service's processes wrote an `output` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Source {
    /// `run.pre_command`, which runs without mirrord.
    PreCommand,
    /// The service's `mirrord exec` or `mirrord container`, with its program.
    Session,
}

/// How far a service's `run.pre_command` got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PreCommandStage {
    Started,
    /// It exited with 0, and the session starts.
    Succeeded,
    /// It exited with another status, and the service doesn't start.
    Failed,
    /// It couldn't be started, e.g. its program isn't on `PATH`, and the service doesn't start.
    SpawnFailed,
}

/// Whether other services still run once a service's pre-command failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Remaining {
    /// The run goes on without the service.
    Others,
    /// It was the last service, so the run ends with its error, which says the same.
    NoneLeft,
}

/// Why a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExitReason {
    /// It exited with 0.
    Succeeded,
    /// Its program ran, then exited with another status.
    Crashed,
    /// It ended before its program ran.
    FailedToStart,
    /// Its program was held for a debugger, which may have ended it.
    WhileDebugged,
}

/// Where a debugged service is when no .NET runtime of it has paused yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ServiceState {
    /// Still running, a while after its program started.
    Running,
    /// Gone.
    Exited,
}

/// One thing `mirrord up` says about its run. See the module's table for the JSON form.
///
/// The variants that pass on a line of a service's output keep that line as `text`, so the text
/// form is the line exactly as the service wrote it.
///
/// # Where the stream ends
///
/// `finished` ends the stream. `exited` ends the service's lifecycle, not its output: until
/// `finished`, a descendant that still holds the session's pipes after the drain can print
/// `output`, and the service's diagnostic port can still report a `debug_hold_ended` or a
/// `debug_note`.
///
/// # Known limit: mirrord's lines are in-band
///
/// A session's progress, debug lines and final error share its stdout and stderr with its
/// program, and are told apart by their text. So a program line that reads as one of them
/// becomes that event: `{"type":"Info","message":"x"}` is `progress`, and a last stderr line of
/// `Error: {"message":"x"}` is `exited`'s `error`. A private channel for mirrord's own lines is
/// the fix, if it's ever needed.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Event<'a> {
    Output {
        service: &'a str,
        source: Source,
        stream: Stream,
        line: &'a str,
    },
    Progress {
        service: &'a str,
        progress: &'a RawValue,
        #[serde(skip)]
        text: &'a str,
    },
    Ready {
        service: &'a str,
    },
    PreCommand {
        service: &'a str,
        stage: PreCommandStage,
        /// The exit status, for `failed`.
        status: Option<String>,
        /// The exit code, for `failed` when there is one: a signal has none.
        code: Option<i32>,
        /// Why it couldn't start, for `spawn_failed`.
        error: Option<String>,
        #[serde(skip)]
        remaining: Remaining,
    },
    DebugPlan {
        service: &'a str,
        plan: &'a RawValue,
        #[serde(skip)]
        text: &'a str,
    },
    DebugHold {
        service: &'a str,
        hold: &'a RawValue,
        #[serde(skip)]
        text: &'a str,
    },
    DebugHoldEnded {
        service: &'a str,
        ended: &'a RawValue,
        #[serde(skip)]
        text: &'a str,
    },
    DebugNote {
        service: &'a str,
        message: &'a str,
    },
    DebugNoRuntimePaused {
        service: &'a str,
        service_state: ServiceState,
    },
    Exited {
        service: &'a str,
        reason: ExitReason,
        status: String,
        code: Option<i32>,
        error: Option<&'a RawValue>,
    },
    OutputError {
        service: &'a str,
        stream: Stream,
        error: String,
    },
    Note {
        service: Option<&'a str>,
        message: &'a str,
    },
    StopFailed {
        error: String,
    },
    Finished {
        error: Option<&'a RawValue>,
    },
}

impl<'a> Event<'a> {
    /// A session's end, with its error when its `mirrord exec` reported one.
    pub(crate) fn exited(
        service: &'a str,
        reason: ExitReason,
        status: ExitStatus,
        error: Option<&'a RawValue>,
    ) -> Self {
        Self::Exited {
            service,
            reason,
            status: status.to_string(),
            code: status.code(),
            error,
        }
    }

    /// A pre-command that started, or succeeded.
    pub(crate) fn pre_command(service: &'a str, stage: PreCommandStage) -> Self {
        Self::PreCommand {
            service,
            stage,
            status: None,
            code: None,
            error: None,
            remaining: Remaining::Others,
        }
    }

    /// A pre-command that exited with `status`, not 0.
    pub(crate) fn pre_command_failed(
        service: &'a str,
        status: ExitStatus,
        remaining: Remaining,
    ) -> Self {
        Self::PreCommand {
            service,
            stage: PreCommandStage::Failed,
            status: Some(status.to_string()),
            code: status.code(),
            error: None,
            remaining,
        }
    }

    /// A pre-command that couldn't be started.
    pub(crate) fn pre_command_spawn_failed(service: &'a str, error: &io::Error) -> Self {
        Self::PreCommand {
            service,
            stage: PreCommandStage::SpawnFailed,
            status: None,
            code: None,
            error: Some(error.to_string()),
            remaining: Remaining::Others,
        }
    }

    /// The line `mirrord up` prints for this event without `--output json`, and the stream it
    /// goes to. `None` for the events that have no line of their own there: a session's
    /// readiness, and the ends of a run that its final error reports.
    pub(crate) fn text(&self) -> Option<(Stream, String)> {
        let stdout = |line: String| Some((Stream::Stdout, line));
        match self {
            // Both of a service's streams go to `up`'s stdout.
            Self::Output { service, line, .. } => stdout(format!("{service}: {line}")),
            Self::Progress { service, text, .. }
            | Self::DebugPlan { service, text, .. }
            | Self::DebugHold { service, text, .. }
            | Self::DebugHoldEnded { service, text, .. } => stdout(format!("{service}: {text}")),
            Self::Ready { .. } | Self::Finished { .. } => None,
            Self::PreCommand {
                service,
                stage,
                status,
                error,
                remaining,
                ..
            } => match stage {
                PreCommandStage::Started => stdout(format!("{service}: running `run.pre_command`")),
                PreCommandStage::Succeeded => {
                    stdout(format!("{service}: `run.pre_command` succeeded, starting"))
                }
                PreCommandStage::Failed if *remaining == Remaining::NoneLeft => None,
                PreCommandStage::Failed => Some((
                    Stream::Stderr,
                    format!(
                        "{service}: `run.pre_command` failed with {}, not starting",
                        status.as_deref().unwrap_or_default()
                    ),
                )),
                PreCommandStage::SpawnFailed => Some((
                    Stream::Stderr,
                    format!(
                        "{service}: `run.pre_command` failed to start: {}",
                        error.as_deref().unwrap_or_default()
                    ),
                )),
            },
            Self::DebugNote { service, message } => stdout(format!("{service}: debug: {message}")),
            Self::DebugNoRuntimePaused {
                service,
                service_state: ServiceState::Running,
            } => stdout(format!("{service}: debug: no .NET runtime paused yet")),
            Self::DebugNoRuntimePaused {
                service,
                service_state: ServiceState::Exited,
            } => stdout(format!(
                "{service}: debug: no .NET runtime paused in this service"
            )),
            Self::Exited {
                service,
                reason: ExitReason::WhileDebugged,
                status,
                ..
            } => stdout(format!(
                "{service}: exited with {status} while debugged, stopping"
            )),
            Self::Exited { .. } => None,
            Self::OutputError { service, error, .. } => {
                Some((Stream::Stderr, format!("{service} output error: {error}")))
            }
            Self::Note {
                service: Some(service),
                message,
            } => stdout(format!("{service}: {message}")),
            Self::Note {
                service: None,
                message,
            } => stdout(message.to_string()),
            Self::StopFailed { error } => Some((
                Stream::Stderr,
                format!("Failed to stop a mirrord up child: {error}"),
            )),
        }
    }

    /// The event as one line of JSON.
    fn json(&self) -> String {
        // Strings, numbers, enums and JSON that already parsed, with no map keys that aren't
        // strings, always serialize.
        serde_json::to_string(self).expect("an up event always serializes to JSON")
    }
}

/// What Rust prints before the error a `main` returns, which the CLI's `main` does: its
/// [`std::process::Termination`] for `Result` writes `Error: {error:?}` to stderr.
const MAIN_ERROR_PREFIX: &str = "Error: ";

/// The final error a session's `mirrord exec` reported on `line`, the last of its stderr, if it
/// is one: [`MAIN_ERROR_PREFIX`] and a miette JSON report.
pub(crate) fn session_error(line: &str) -> Option<Box<RawValue>> {
    let report = line.strip_prefix(MAIN_ERROR_PREFIX)?;
    let fields = serde_json::from_str::<Map<String, Value>>(report).ok()?;
    fields
        .get("message")
        .is_some_and(Value::is_string)
        .then(|| RawValue::from_string(report.to_owned()).ok())
        .flatten()
}

/// The stderr line that [`session_error`] read `report` from, as the session printed it.
pub(crate) fn session_error_line(report: &RawValue) -> String {
    format!("{MAIN_ERROR_PREFIX}{}", report.get())
}

/// `error` as miette's JSON report, the shape `mirrord ls` prints its error in.
fn report_json(error: &dyn Diagnostic) -> Box<RawValue> {
    let mut report = String::new();
    let rendered = JSONReportHandler::new().render_report(&mut report, error);
    match rendered
        .ok()
        .and_then(|()| RawValue::from_string(report).ok())
    {
        Some(report) => report,
        // A report always renders to a `String`, as valid JSON. Should it not, the message is
        // still worth more than nothing.
        None => to_raw_value(&serde_json::json!({ "message": error.to_string() }))
            .expect("an object with one string field always serializes"),
    }
}

/// Writes `mirrord up`'s [`Event`]s in its [`OutputFormat`].
///
/// Each event is one `println!` (or `eprintln!`), which holds the stream's lock for the whole
/// line, so the lines of services printing at once never interleave.
#[derive(Debug, Clone)]
pub struct Printer {
    format: OutputFormat,
    sink: Sink,
}

/// Where a [`Printer`]'s lines go.
#[derive(Debug, Clone)]
enum Sink {
    /// `mirrord up`'s own stdout and stderr.
    Terminal,
    /// A list a test reads.
    #[cfg(test)]
    Captured(Arc<Mutex<Vec<Printed>>>),
}

/// A line a test's [`Printer`] printed.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Printed {
    pub(crate) stream: Stream,
    pub(crate) line: String,
}

impl Printer {
    /// Writes to `mirrord up`'s stdout and stderr, in `format`.
    pub fn new(format: OutputFormat) -> Self {
        Self {
            format,
            sink: Sink::Terminal,
        }
    }

    /// Keeps every line, for a test to read.
    #[cfg(test)]
    pub(crate) fn captured(format: OutputFormat) -> (Self, Arc<Mutex<Vec<Printed>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let printer = Self {
            format,
            sink: Sink::Captured(lines.clone()),
        };
        (printer, lines)
    }

    /// The format it writes.
    pub fn format(&self) -> OutputFormat {
        self.format
    }

    pub(crate) fn print(&self, event: &Event<'_>) {
        let printed = match self.format {
            OutputFormat::Text => event.text(),
            OutputFormat::Json => Some((Stream::Stdout, event.json())),
        };
        let Some((stream, line)) = printed else {
            return;
        };
        match (&self.sink, stream) {
            (Sink::Terminal, Stream::Stdout) => println!("{line}"),
            (Sink::Terminal, Stream::Stderr) => eprintln!("{line}"),
            #[cfg(test)]
            (Sink::Captured(lines), stream) => lines
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(Printed { stream, line }),
        }
    }

    /// Something `mirrord up` says before the services start, about `service` when it names one.
    pub fn note(&self, service: Option<&str>, message: &str) {
        self.print(&Event::Note { service, message });
    }

    /// Says the run is over, failed with `error` when there is one. Only `--output json` prints
    /// it, as the last line; without the flag, the CLI prints the error itself.
    pub fn finished(&self, error: Option<&dyn Diagnostic>) {
        let error = error.map(report_json);
        self.print(&Event::Finished {
            error: error.as_deref(),
        });
    }

    /// Whether `up` may ask the user something, which only text output allows: an IDE reading
    /// JSON can't answer, and a prompt would put text on its stdout.
    pub fn can_prompt(&self) -> bool {
        self.format == OutputFormat::Text
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use rstest::rstest;
    use serde_json::Value;

    use super::*;
    use crate::UpError;

    /// Without `--output json`, each event is the line `mirrord up` printed before events
    /// existed, on the same stream.
    #[rstest]
    // A line of the program's stdout, behind the service's name.
    #[case::session_stdout(
        Event::Output { service: "api", source: Source::Session, stream: Stream::Stdout, line: "hello" },
        (Stream::Stdout, "api: hello")
    )]
    // Both of a service's streams go to `up`'s stdout.
    #[case::session_stderr(
        Event::Output { service: "api", source: Source::Session, stream: Stream::Stderr, line: "oops" },
        (Stream::Stdout, "api: oops")
    )]
    // The pre-command's start and its success go to stdout.
    #[case::pre_command_started(
        Event::pre_command("api", PreCommandStage::Started),
        (Stream::Stdout, "api: running `run.pre_command`")
    )]
    // The same line `up` printed before the session starts.
    #[case::pre_command_succeeded(
        Event::pre_command("api", PreCommandStage::Succeeded),
        (Stream::Stdout, "api: `run.pre_command` succeeded, starting")
    )]
    // A failure goes to stderr, while the other services run on.
    #[case::pre_command_failed(
        Event::PreCommand {
            service: "api",
            stage: PreCommandStage::Failed,
            status: Some("exit code: 2".to_owned()),
            code: Some(2),
            error: None,
            remaining: Remaining::Others,
        },
        (Stream::Stderr, "api: `run.pre_command` failed with exit code: 2, not starting")
    )]
    // A pre-command that can't start goes to stderr too, with why.
    #[case::pre_command_spawn_failed(
        Event::pre_command_spawn_failed("api", &std::io::Error::other("program not found")),
        (Stream::Stderr, "api: `run.pre_command` failed to start: program not found")
    )]
    // A refused runtime connection, behind `debug: `.
    #[case::debug_note(
        Event::DebugNote { service: "api", message: "a runtime of another process connected" },
        (Stream::Stdout, "api: debug: a runtime of another process connected")
    )]
    // The odd one out: the one line text mode gains, while a debugged service runs.
    #[case::no_runtime_paused_yet(
        Event::DebugNoRuntimePaused { service: "api", service_state: ServiceState::Running },
        (Stream::Stdout, "api: debug: no .NET runtime paused yet")
    )]
    // The line `up` printed once a debugged service exited with no runtime paused.
    #[case::no_runtime_paused_at_exit(
        Event::DebugNoRuntimePaused { service: "api", service_state: ServiceState::Exited },
        (Stream::Stdout, "api: debug: no .NET runtime paused in this service")
    )]
    // The only end with a line of its own: the run ends without an error.
    #[case::exited_while_debugged(
        Event::Exited {
            service: "api",
            reason: ExitReason::WhileDebugged,
            status: "exit code: 1".to_owned(),
            code: Some(1),
            error: None,
        },
        (Stream::Stdout, "api: exited with exit code: 1 while debugged, stopping")
    )]
    // A stream that couldn't be read goes to stderr, without the `: ` after the name.
    #[case::output_error(
        Event::OutputError { service: "api", stream: Stream::Stdout, error: "pipe broke".to_owned() },
        (Stream::Stderr, "api output error: pipe broke")
    )]
    // A note before the services start, about one of them.
    #[case::note_of_a_service(
        Event::Note { service: Some("api"), message: "requesting DB branching" },
        (Stream::Stdout, "api: requesting DB branching")
    )]
    // A note about the whole run has no name in front.
    #[case::note_of_the_run(
        Event::Note { service: None, message: "DB branch `users` is requested from more than one namespace" },
        (Stream::Stdout, "DB branch `users` is requested from more than one namespace")
    )]
    // A service that couldn't be stopped, on stderr.
    #[case::stop_failed(
        Event::StopFailed { error: "access denied".to_owned() },
        (Stream::Stderr, "Failed to stop a mirrord up child: access denied")
    )]
    fn an_event_prints_its_old_line_in_text(
        #[case] event: Event<'_>,
        #[case] (stream, line): (Stream, &str),
    ) {
        assert_eq!(
            event.text(),
            Some((stream, line.to_owned())),
            "text output prints the line `up` printed before, on the same stream"
        );
    }

    /// Without `--output json`, the events that only carry state print nothing: the session's
    /// own `Ready!` line says it, or the run's final error does.
    #[rstest]
    // The session's own `Ready!` line says it.
    #[case::ready(Event::Ready { service: "api" })]
    // A crash and a failed start are the run's final error.
    #[case::crashed(Event::Exited {
        service: "api",
        reason: ExitReason::Crashed,
        status: "exit code: 3".to_owned(),
        code: Some(3),
        error: None,
    })]
    // The same, for a session that ended before its program ran.
    #[case::failed_to_start(Event::Exited {
        service: "api",
        reason: ExitReason::FailedToStart,
        status: "exit code: 1".to_owned(),
        code: Some(1),
        error: None,
    })]
    // The last service's pre-command failed, so the run's error says it.
    #[case::last_pre_command_failed(Event::PreCommand {
        service: "api",
        stage: PreCommandStage::Failed,
        status: Some("exit code: 2".to_owned()),
        code: Some(2),
        error: None,
        remaining: Remaining::NoneLeft,
    })]
    // The CLI prints the run's error itself.
    #[case::finished(Event::Finished { error: None })]
    fn an_event_with_no_old_line_prints_nothing_in_text(#[case] event: Event<'_>) {
        assert_eq!(event.text(), None, "{event:?} had no line of its own");
    }

    /// The last line of a run with an error is `finished`, with the error as miette's JSON
    /// report, on stdout. Text output leaves it to the CLI, which prints the error itself.
    #[test]
    fn finished_carries_the_error_as_a_report() {
        let error = UpError::ServiceFailedToStart {
            name: Arc::from("api"),
        };
        let (json, json_lines) = Printer::captured(OutputFormat::Json);
        let (text, text_lines) = Printer::captured(OutputFormat::Text);

        json.finished(Some(&error));
        text.finished(Some(&error));

        let json_lines = json_lines.lock().expect("no test thread panicked");
        let [Printed { stream, line }] = json_lines.as_slice() else {
            panic!("finished is one line: {json_lines:?}");
        };
        assert_eq!(*stream, Stream::Stdout, "every JSON event goes to stdout");
        let finished: Value = serde_json::from_str(line).expect("finished is JSON");
        assert_eq!(finished["type"], "finished", "the run's last event");
        assert_eq!(
            finished["error"]["message"], "Service api failed to start",
            "the report carries the run's error"
        );
        assert_eq!(
            finished["error"]["severity"], "error",
            "the report is miette's, with its severity"
        );
        assert!(
            text_lines
                .lock()
                .expect("no test thread panicked")
                .is_empty(),
            "text output prints no finished line"
        );
    }

    /// A run that ended without an error finishes with `"error":null`.
    #[test]
    fn finished_without_an_error_is_null() {
        let (json, lines) = Printer::captured(OutputFormat::Json);
        json.finished(None);
        assert_eq!(
            lines.lock().expect("no test thread panicked").as_slice(),
            [Printed {
                stream: Stream::Stdout,
                line: r#"{"type":"finished","error":null}"#.to_owned(),
            }],
            "a run without an error still ends with `finished`"
        );
    }

    /// Whether a session's stderr line is its `mirrord exec`'s final error.
    #[derive(Debug, PartialEq, Eq)]
    enum Read {
        /// The line is the session's final error.
        Report,
        /// The line is the program's, passed on as output.
        NotAnError,
    }

    /// Only `Error: ` and a JSON report with a `message`, as `main` prints a miette JSON error,
    /// is a session's final error.
    #[rstest]
    // The error line of a real `mirrord ls -f` run with a bad config.
    #[case::real_report(include_str!("testdata/output/ls-error.txt"), Read::Report)]
    // A program's own JSON log line, which `main` didn't print.
    #[case::json_without_the_prefix(
        r#"{"message":"listening","severity":"error"}"#,
        Read::NotAnError
    )]
    // A text error, as a program or a session without the JSON hook prints it.
    #[case::prefix_without_json("Error: connection refused", Read::NotAnError)]
    // JSON, but not miette's report: it has no `message`.
    #[case::report_without_a_message(r#"Error: {"code":1}"#, Read::NotAnError)]
    // A `message` miette would never write, since it's not a string.
    #[case::message_that_is_not_a_string(r#"Error: {"message":7}"#, Read::NotAnError)]
    fn only_a_report_line_is_a_session_error(#[case] line: &str, #[case] expected: Read) {
        let read = match session_error(line.trim_end()) {
            Some(_) => Read::Report,
            None => Read::NotAnError,
        };
        assert_eq!(read, expected, "the line was {line:?}");
    }
}
