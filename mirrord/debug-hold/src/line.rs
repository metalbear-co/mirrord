//! The lines an IDE reads from `mirrord up`'s output to find holds and their ends.
//!
//! Each is a fixed word, a space, and one JSON object on the same line:
//!
//! - `debug-hold {…}`: a program is held, and the IDE may attach to it, then release it
//!   ([`Announcement`]).
//! - `debug-hold-ended {…}`: a hold is over ([`Ended`]).
//!
//! `mirrord up` prefixes every line with the service's name and `: `, like the rest of the
//! service's output. The objects carry [`LINE_VERSION`] as `v`. Fields are only ever added, so an
//! IDE ignores the ones it doesn't know, and every field but `v`, `trigger`, `pid` and `release`
//! may be `null`.

use std::fmt;

use serde::Serialize;

/// The version of the lines' JSON, the `v` of every object.
pub const LINE_VERSION: u32 = 2;

/// How [`Announcement`]'s line starts, before its JSON.
pub const ANNOUNCEMENT_PREFIX: &str = "debug-hold ";

/// How [`Ended`]'s line starts, before its JSON.
pub const ENDED_PREFIX: &str = "debug-hold-ended ";

/// What holds the program, which tells the IDE when its attach can complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    /// The process is held before its entry point: suspended at creation on Windows, in the
    /// layer's constructor on Linux and macOS. `mirrord exec` holds the program it starts.
    Process,
    /// A .NET runtime paused at its startup, before any managed code, on the diagnostic port
    /// `mirrord up` serves.
    Runtime,
}

/// What is known about a held process, from the OS or from whoever started it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HeldProcess {
    /// The image's full path.
    pub image: Option<String>,
    /// The process that started it.
    pub parent_pid: Option<u32>,
    /// Its command line, as one string.
    pub command_line: Option<String>,
}

/// What a paused .NET runtime says about itself. Every field is `None` for a process hold.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RuntimeFacts {
    /// The runtime instance's cookie, a GUID new in every process.
    pub runtime: Option<String>,
    /// The managed entry assembly's name without `.dll`: the app's own, or `dotnet` for the SDK.
    pub entry_assembly: Option<String>,
    /// The runtime's product version.
    pub runtime_version: Option<String>,
    /// The OS the runtime reports.
    pub os: Option<String>,
    /// The architecture the runtime reports.
    pub arch: Option<String>,
    /// The runtime identifier, such as `win-x64`.
    pub rid: Option<String>,
}

/// `debug-hold {…}`: a held program the IDE attaches to, then releases.
///
/// The IDE releases it through `release`, the same way for both triggers: on Windows it opens the
/// event with `EVENT_MODIFY_STATE` and sets it, and on Linux and macOS it writes one byte to the
/// FIFO.
///
/// A release means "the IDE's debugger is ready for the program to go on", which isn't always
/// "attached": a debugger may finish attaching to a paused runtime only once the runtime runs
/// again. The IDE releases every hold it gets, attached or not, since one it never releases stays
/// held until `mirrord up` stops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Announcement {
    /// Always [`LINE_VERSION`].
    v: u32,
    /// What holds the program.
    pub trigger: Trigger,
    /// The held process.
    pub pid: u32,
    /// What the IDE opens to release it. See [`crate::ReleaseGate::release_name`].
    pub release: String,
    /// What the OS says about the held process.
    #[serde(flatten)]
    pub process: HeldProcess,
    /// What a paused runtime says about itself.
    #[serde(flatten)]
    pub runtime: RuntimeFacts,
}

impl Announcement {
    /// The announcement of a process hold.
    pub fn process(pid: u32, release: String, process: HeldProcess) -> Self {
        Self {
            v: LINE_VERSION,
            trigger: Trigger::Process,
            pid,
            release,
            process,
            runtime: RuntimeFacts::default(),
        }
    }

    /// The announcement of a .NET runtime paused at its startup.
    pub fn runtime(pid: u32, release: String, process: HeldProcess, runtime: RuntimeFacts) -> Self {
        Self {
            v: LINE_VERSION,
            trigger: Trigger::Runtime,
            pid,
            release,
            process,
            runtime,
        }
    }
}

/// The whole line, without its newline: `debug-hold {…}`.
impl fmt::Display for Announcement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_line(f, ANNOUNCEMENT_PREFIX, self)
    }
}

/// How a hold ended, in [`Ended`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EndedOutcome {
    /// Released, so the program goes on.
    Resumed,
    /// The process ended while it was held.
    Exited,
    /// The hold broke: the program may stay held until `mirrord up` stops. `error` says why.
    Failed,
}

/// `debug-hold-ended {…}`: a hold is over, and its `release` is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ended {
    /// Always [`LINE_VERSION`].
    v: u32,
    /// The release of the [`Announcement`] it ends.
    pub release: String,
    /// The process that was held.
    pub pid: u32,
    /// How it ended.
    pub outcome: EndedOutcome,
    /// Why, when it [failed](EndedOutcome::Failed).
    pub error: Option<String>,
}

impl Ended {
    /// The end of the hold `release` names.
    pub fn new(pid: u32, release: String, outcome: EndedOutcome, error: Option<String>) -> Self {
        Self {
            v: LINE_VERSION,
            release,
            pid,
            outcome,
            error,
        }
    }
}

/// The whole line, without its newline: `debug-hold-ended {…}`.
impl fmt::Display for Ended {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_line(f, ENDED_PREFIX, self)
    }
}

fn write_line(f: &mut fmt::Formatter<'_>, prefix: &str, value: &impl Serialize) -> fmt::Result {
    // Strings and numbers only, so serializing can't fail; JSON escapes every newline in them.
    let json = serde_json::to_string(value).map_err(|_| fmt::Error)?;
    write!(f, "{prefix}{json}")
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use serde_json::Value;

    use super::*;

    fn json_of(line: &str, prefix: &str) -> Value {
        let json = line
            .strip_prefix(prefix)
            .unwrap_or_else(|| panic!("the line starts with {prefix:?}: {line}"));
        serde_json::from_str(json).unwrap_or_else(|error| panic!("{error}: {json}"))
    }

    /// An IDE reads every field the contract names, `null` included, and a path with spaces,
    /// which the earlier `key=value` line couldn't carry, arrives whole.
    #[test]
    fn a_runtime_announcement_carries_every_field_on_one_line() {
        let announcement = Announcement::runtime(
            9872,
            r"Local\mirrord-debug-release-9872-3f2a".to_owned(),
            HeldProcess {
                image: Some(r"C:\Program Files\dotnet\dotnet.exe".to_owned()),
                parent_pid: Some(39552),
                command_line: Some("\"dotnet\" App.dll\n--urls x".to_owned()),
            },
            RuntimeFacts {
                runtime: Some("6ea9bcc5-d70f-43e4-a5a5-abafb0c22dc7".to_owned()),
                entry_assembly: Some("App".to_owned()),
                runtime_version: Some("8.0.31".to_owned()),
                os: Some("Windows".to_owned()),
                arch: Some("x64".to_owned()),
                rid: None,
            },
        );

        let line = announcement.to_string();

        assert!(
            !line.contains('\n'),
            "one line, whatever the fields hold: {line}"
        );
        assert_eq!(
            json_of(&line, ANNOUNCEMENT_PREFIX),
            serde_json::json!({
                "v": 2,
                "trigger": "runtime",
                "pid": 9872,
                "release": r"Local\mirrord-debug-release-9872-3f2a",
                "image": r"C:\Program Files\dotnet\dotnet.exe",
                "parent_pid": 39552,
                "command_line": "\"dotnet\" App.dll\n--urls x",
                "runtime": "6ea9bcc5-d70f-43e4-a5a5-abafb0c22dc7",
                "entry_assembly": "App",
                "runtime_version": "8.0.31",
                "os": "Windows",
                "arch": "x64",
                "rid": null,
            }),
            "the IDE's parser expects exactly these fields"
        );
    }

    /// A process hold has no runtime to report, but the fields are still there, as `null`, so an
    /// IDE reads one shape for both triggers.
    #[test]
    fn a_process_announcement_has_null_runtime_fields() {
        let line =
            Announcement::process(42, "release-42".to_owned(), HeldProcess::default()).to_string();

        let json = json_of(&line, ANNOUNCEMENT_PREFIX);
        assert_eq!(json["trigger"], "process");
        for field in [
            "runtime",
            "entry_assembly",
            "runtime_version",
            "rid",
            "image",
        ] {
            assert_eq!(
                json[field],
                Value::Null,
                "{field} is present, and null: {line}"
            );
        }
    }

    /// The ended line names the hold by its release, so an IDE holding several can tell which one
    /// ended.
    #[test]
    fn an_ended_line_names_its_release_and_outcome() {
        let line = Ended::new(
            42,
            "release-42".to_owned(),
            EndedOutcome::Failed,
            Some("broken pipe".to_owned()),
        )
        .to_string();

        assert_eq!(
            json_of(&line, ENDED_PREFIX),
            serde_json::json!({
                "v": 2,
                "release": "release-42",
                "pid": 42,
                "outcome": "failed",
                "error": "broken pipe",
            })
        );
        assert!(
            !line.starts_with(ANNOUNCEMENT_PREFIX),
            "an IDE matching `debug-hold ` must never read an end as a new hold: {line}"
        );
    }
}
