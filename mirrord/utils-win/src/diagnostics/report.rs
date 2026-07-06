//! The crash report.
//!
//! The monitor composes this after a dump or a detected death — the safe place for it; the crashing
//! process never builds a report.
//!
//! One incident produces a set of flat files in the crash directory, all sharing the same stem so
//! they cluster together:
//!
//! - `<stem>.report.txt` — the short, non-scary summary the user reads and copies. It is the source
//!   of truth, logged to the console and shown in the native [`dialog`](super::dialog).
//! - `<stem>.record.txt` — the raw in-process crash record (code, address, faulting module, stack),
//!   when the handler captured one.
//! - `<stem>.zip` — the one file to attach: the report, the crash record, the module inventory, the
//!   session's layer logs, and the minidump when dumps are enabled. The dump lives only here.
//!
//! The report text and the console output are always written, so a dismissed dialog loses nothing.
//! The dialog is shown once per session (suppressed under `CI`); it blocks until the user dismisses
//! it, and the monitor's watchdog keeps the monitor alive until then.

use std::{
    fmt::Write as _,
    fs::File,
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use winapi::shared::{
    ntdef::{NT_ERROR, NTSTATUS},
    ntstatus,
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

/// Set while the crash dialog is on screen, blocking on the user.
///
/// The monitor's lifetime watchdog reads it so the monitor outlives the CLI session until the
/// dialog is dismissed, instead of being torn down the instant the crashed process exits.
static DIALOG_SHOWING: AtomicBool = AtomicBool::new(false);

/// Whether the crash dialog is currently on screen.
///
/// # Returns
///
/// `true` while a dialog is open and waiting on the user.
pub(crate) fn dialog_showing() -> bool {
    DIALOG_SHOWING.load(Ordering::SeqCst)
}

/// Where to file a bug report.
const GITHUB_NEW_ISSUE_URL: &str = "https://github.com/metalbear-co/mirrord/issues/new/choose";
/// Where to reach the team directly.
const METALBEAR_SLACK_URL: &str = "https://metalbear.com/slack";
/// Where to reach the team by email.
const METALBEAR_EMAIL: &str = "hi@metalbear.com";
/// The home of the witty-crash-comment tradition, linked under the quote (clickable in the dialog).
const CRASH_WIKI_URL: &str = "https://minecraft.wiki/w/Crash";

/// Light one-liners shown at the top of the report, in the spirit of Minecraft's crash screen
/// ([`CRASH_WIKI_URL`]). One is picked per incident by the focused pid, so it varies between
/// crashes but stays stable for a given report. ASCII only, so it never mojibakes on a legacy
/// console.
const WITTY_COMMENTS: &[&str] = &[
    "But it worked in the cluster!",
    "Don't panic. (We did, a little.)",
    "This one's on us, not your code.",
    "Somewhere, a packet took a wrong turn.",
    "It's not a bug, it's an unhandled feature.",
    "Quite honestly, we wouldn't worry ourselves about that.",
    "Have you tried turning the pod off and on again?",
    "Surprise! Well, this is awkward.",
    "We'll get this sorted, promise.",
    "Works on our machine too, sadly.",
    "Hold my kubeconfig.",
    "The packets were coming from inside the house.",
];

/// NTSTATUS codes we name, as `u32` to match the process exit / `ExceptionCode` values we compare
/// against. winapi types `NTSTATUS` as `i32`, so each aliases the matching `ntstatus` constant.
const STATUS_ACCESS_VIOLATION: u32 = ntstatus::STATUS_ACCESS_VIOLATION as u32;
const STATUS_IN_PAGE_ERROR: u32 = ntstatus::STATUS_IN_PAGE_ERROR as u32;
const STATUS_ILLEGAL_INSTRUCTION: u32 = ntstatus::STATUS_ILLEGAL_INSTRUCTION as u32;
const STATUS_PRIVILEGED_INSTRUCTION: u32 = ntstatus::STATUS_PRIVILEGED_INSTRUCTION as u32;
const STATUS_INTEGER_DIVIDE_BY_ZERO: u32 = ntstatus::STATUS_INTEGER_DIVIDE_BY_ZERO as u32;
const STATUS_STACK_OVERFLOW: u32 = ntstatus::STATUS_STACK_OVERFLOW as u32;
const STATUS_HEAP_CORRUPTION: u32 = ntstatus::STATUS_HEAP_CORRUPTION as u32;
/// Stack-buffer-overrun fast-fail — what `abort()` / `__fastfail` raise (0xC0000409).
const STATUS_STACK_BUFFER_OVERRUN: u32 = ntstatus::STATUS_STACK_BUFFER_OVERRUN as u32;
/// Ctrl-C exit. User/application-initiated — not a mirrord crash. It carries no customer bit, so it
/// is named explicitly.
const STATUS_CONTROL_C_EXIT: u32 = ntstatus::STATUS_CONTROL_C_EXIT as u32;

/// The NTSTATUS customer / application-defined bit (bit 29).
///
/// An NTSTATUS packs severity in the top two bits and an application-defined flag in bit 29. The OS
/// leaves bit 29 clear on its own faults (access violation, stack overflow, divide by zero, …) and
/// sets it on codes a runtime defines for itself.
///
/// We use it to tell "the OS faulted the process" from "a runtime raised its own exception" without
/// enumerating every runtime: a managed .NET exception (`0xE0434352`) and a C++ EH throw
/// (`0xE06D7363`) both carry the bit, so both are treated as application-level, not mirrord
/// crashes.
const STATUS_APPLICATION_BIT: u32 = 0x2000_0000;

/// One process in the session, as seen by the monitor's registrations.
///
/// A node is kept for the whole session — a process that has already died stays in the tree so the
/// ancestry around a crash is legible (the classic case: a parent that died before its child, the
/// init-failure race). [`exit_code`](Self::exit_code) records that death.
#[derive(Debug, Clone)]
pub struct ProcessNode {
    /// The process id.
    pub pid: u32,
    /// Its parent's id.
    pub parent_pid: u32,
    /// Its image name.
    pub name: String,
    /// Its session-role label.
    pub role: String,
    /// Its exact layer log file, when file logging is active. Registered so the bundle never
    /// globs.
    pub log_path: Option<PathBuf>,
    /// The exit code observed when this process died, or `None` if it was still alive — or its
    /// death had not yet been observed — when the report was built. A `Some` marks the node
    /// dead in the tree.
    pub exit_code: Option<u32>,
}

/// How the focused process ended.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// A handled crash. A dump was produced.
    Crashed,
    /// The process died with no handler firing. No dump is possible.
    Terminated {
        /// The process exit code.
        exit_code: u32,
    },
    /// The layer failed to initialize before the process could run under mirrord.
    ///
    /// Not a crash and not a kill — a controlled init error (most often the parent died and the
    /// init event was deleted before this child could open it, see `sync.rs`). Surfaced so it
    /// is not lost as a silent clean exit.
    InitFailed {
        /// A human-readable reason, including the session role and parent liveness.
        reason: String,
    },
}

/// Whether an exit code is an application-level termination mirrord stays silent on.
///
/// A runtime-defined exception — any error-severity NTSTATUS with the customer bit set, which
/// covers a managed .NET exception (`0xE0434352`), a C++ EH throw (`0xE06D7363`), and any other
/// runtime code — or a Ctrl-C. These are the runtime's or the user's own business, reported through
/// the runtime, not by us, so even a non-clean death carrying one is not surfaced.
pub(crate) fn is_app_level_exit(code: u32) -> bool {
    code == STATUS_CONTROL_C_EXIT
        || (NT_ERROR(code as NTSTATUS) && code & STATUS_APPLICATION_BIT != 0)
}

/// Everything the report needs about one crash or death.
pub struct CrashReport<'a> {
    /// The crashed or dead process.
    pub focus_pid: u32,
    /// Its image name.
    pub focus_name: &'a str,
    /// How it ended.
    pub outcome: Outcome,
    /// Every registered process in the session.
    pub nodes: &'a [ProcessNode],
    /// The root CLI process id.
    pub root_pid: u32,
    /// The minidump path, when one was produced.
    pub dump: Option<&'a Path>,
    /// The text crash record path, when one exists.
    pub record: Option<&'a Path>,
    /// The exact session log files to bundle, gathered from the registrations.
    pub logs: &'a [PathBuf],
    /// A module inventory of the crashing process, when it could be enumerated.
    pub modules: Option<&'a str>,
    /// The mirrord version string.
    pub mirrord_version: &'a str,
    /// The shared incident stem all of this incident's artifact file names are built from.
    pub stem: &'a str,
}

/// Composes the report, writes its flat artifacts, and shows the dialog.
///
/// The report text is always written and logged. The `.zip` is built best-effort.
///
/// # Arguments
///
/// * `report` - the crash facts.
/// * `directory` - where to write the artifacts.
/// * `show_dialog` - whether this caller claimed the single per-session dialog slot. The caller
///   does the claiming so this function never has to hold a shared lock across the blocking dialog.
pub fn surface(report: &CrashReport, directory: &Path, show_dialog: bool) {
    let report_text = report_text(report);

    // The report text file is the source of truth.
    write_report(directory, report, &report_text);

    // Log it so it appears in the monitor's console too.
    tracing::warn!("{report_text}");

    let archive = build_archive(directory, report, &report_text)
        .inspect_err(|error| tracing::warn!(%error, "crash report: failed to build the archive"))
        .ok()
        .flatten();
    if let Some(archive) = &archive {
        tracing::info!(path = %archive.display(), "crash report: wrote the attachable archive");
    }

    // Show the dialog whenever this caller claimed the session's single slot — unless we're in CI,
    // where nobody could dismiss it and the monitor would block. It blocks until the user dismisses
    // it, and the monitor's watchdog keeps the monitor alive until then. The file and the console
    // output are always written, so a suppressed or dismissed dialog loses nothing.
    if show_dialog && !ci_info::is_ci() {
        let title = dialog_title(report);
        let subtitle = dialog_subtitle(report);
        let body = dialog_body(report, &report_text);

        DIALOG_SHOWING.store(true, Ordering::SeqCst);
        super::dialog::show(&title, &subtitle, &body, directory, GITHUB_NEW_ISSUE_URL);
        DIALOG_SHOWING.store(false, Ordering::SeqCst);
    }
}

/// Whether the focused process should be presented as a crash (vs. an external kill).
fn presented_as_crash(report: &CrashReport) -> bool {
    matches!(&report.outcome, Outcome::Crashed)
        || matches!(&report.outcome, Outcome::Terminated { exit_code } if is_crash_code(*exit_code))
}

/// The bold title line shown in the dialog's header band. Kept short so it never clips.
fn dialog_title(report: &CrashReport) -> String {
    if matches!(report.outcome, Outcome::InitFailed { .. }) {
        "mirrord layer failed to start".to_owned()
    } else if presented_as_crash(report) {
        "mirrord caught a crash".to_owned()
    } else {
        "A process was terminated".to_owned()
    }
}

/// The lighter subtitle line: the process, plus a named exception when we have one.
fn dialog_subtitle(report: &CrashReport) -> String {
    let mut subtitle = format!("{} (pid {})", report.focus_name, report.focus_pid);
    if let Outcome::Terminated { exit_code } = &report.outcome
        && let Some(name) = exception_name(*exit_code)
    {
        let _ = write!(subtitle, " · {name} ({exit_code:#010x})");
    }
    subtitle
}

/// Builds the dialog's scrollable body: the friendly report first, then the full technical crash
/// record (code, address, faulting module, stack) as an appendix at the bottom for whoever digs in.
fn dialog_body(report: &CrashReport, report_text: &str) -> String {
    let mut body = String::from(report_text);
    if let Some(record) = report.record
        && let Ok(text) = std::fs::read_to_string(record)
    {
        body.push('\n');
        body.push_str(text.trim_end());
        body.push('\n');
    }
    body
}

/// Builds the ancestry tree, drawn with box characters like the `tree` command.
///
/// The mirrord CLI root is the header line; every registered process hangs beneath it with
/// `├─`/`└─` connectors. A process launched directly by the CLI registers with `parent_pid` 0, and
/// a process whose parent already died without registering has an untracked parent — both hang
/// directly under the root rather than floating as separate roots. The focused process is marked.
pub fn format_tree(report: &CrashReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "mirrord({})", report.root_pid);

    let known: Vec<u32> = report.nodes.iter().map(|node| node.pid).collect();
    let roots: Vec<&ProcessNode> = report
        .nodes
        .iter()
        .filter(|node| node.parent_pid == report.root_pid || !known.contains(&node.parent_pid))
        .collect();
    write_children(&mut out, report, &roots, "", 0);

    out
}

/// Deepest ancestry level walked. Bounds recursion in case recycled pids form a parent cycle, which
/// would otherwise overflow the watcher thread's stack and take down the monitor.
const MAX_TREE_DEPTH: usize = 64;

/// Writes `children` with `tree`-style connectors, recursing into each one's own children.
///
/// # Arguments
///
/// * `out` - the buffer to append to.
/// * `report` - the crash facts, for the focus mark.
/// * `children` - the nodes to draw at this level, in order.
/// * `prefix` - the accumulated indent (pipes and spaces) carried from ancestors.
/// * `depth` - the current recursion depth, bounded by [`MAX_TREE_DEPTH`].
fn write_children(
    out: &mut String,
    report: &CrashReport,
    children: &[&ProcessNode],
    prefix: &str,
    depth: usize,
) {
    if depth >= MAX_TREE_DEPTH {
        return;
    }

    let last = children.len().saturating_sub(1);
    for (index, node) in children.iter().enumerate() {
        let is_last = index == last;
        let connector = if is_last { "└─ " } else { "├─ " };
        let _ = writeln!(out, "{prefix}{connector}{}", node_label(report, node));

        // A node's children name it as their parent; the self-loop guard stops a recycled pid that
        // lists itself as its own parent from recursing forever.
        let grandchildren: Vec<&ProcessNode> = report
            .nodes
            .iter()
            .filter(|child| child.parent_pid == node.pid && child.pid != node.pid)
            .collect();
        let child_prefix = format!("{prefix}{}", if is_last { "   " } else { "│  " });
        write_children(
            out,
            report,
            &grandchildren,
            &child_prefix,
            depth.saturating_add(1),
        );
    }
}

/// Formats one node: `name(pid) [role]`, with the crash mark on the focused process and a dead
/// marker on any other process that already exited this session.
fn node_label(report: &CrashReport, node: &ProcessNode) -> String {
    let mark = if node.pid == report.focus_pid {
        focus_mark(report).to_owned()
    } else if let Some(exit_code) = node.exit_code {
        // Already dead earlier in the session — kept in the tree, marked so the ancestry is legible
        // (e.g. a parent that died before its child could finish initializing).
        format!(" [exited {exit_code:#010x}]")
    } else {
        String::new()
    };
    format!("{}({}) [{}]{mark}", node.name, node.pid, node.role)
}

/// The mark drawn on the focused process, naming how it ended.
fn focus_mark(report: &CrashReport) -> &'static str {
    match &report.outcome {
        Outcome::Crashed => " ✗ crashed",
        Outcome::InitFailed { .. } => " ✗ failed to start",
        Outcome::Terminated { exit_code } if is_crash_code(*exit_code) => " ✗ crashed",
        Outcome::Terminated { .. } => " ✗ terminated",
    }
}

/// Whether a code is a native CPU/OS fault mirrord owns.
///
/// Every error-severity NTSTATUS ([`NT_ERROR`], the top two bits set, i.e. the old `>= 0xC0000000`)
/// with the customer/application bit clear. The OS leaves that bit clear on its own faults (access
/// violation, stack overflow, divide by zero, in-page error, …) and sets it on runtime-defined
/// exceptions, so this catches *unnamed* native faults while excluding a managed .NET exception
/// (`0xE0434352`) or a C++ EH throw (`0xE06D7363`).
///
/// This is the one predicate shared by both sides: the in-process handler gates on it directly (the
/// only codes the unhandled filter ever sees are SEH exception codes), and the monitor's
/// [`is_crash_code`] builds on it for the exit codes it sees.
pub(crate) fn is_native_fault(code: u32) -> bool {
    NT_ERROR(code as NTSTATUS) && code & STATUS_APPLICATION_BIT == 0
}

/// Whether an exit code indicates a real native fault we present as a crash.
///
/// A native fault ([`is_native_fault`]) that is not a Ctrl-C. The two predicates differ only on
/// `STATUS_CONTROL_C_EXIT`: it is an *exit* code the monitor sees (and must not present as a
/// crash), never an *exception* code the in-process handler sees.
fn is_crash_code(code: u32) -> bool {
    is_native_fault(code) && code != STATUS_CONTROL_C_EXIT
}

/// Returns a short name for a known NTSTATUS crash code.
fn exception_name(code: u32) -> Option<&'static str> {
    match code {
        STATUS_ACCESS_VIOLATION => Some("access violation"),
        STATUS_IN_PAGE_ERROR => Some("in-page error"),
        STATUS_ILLEGAL_INSTRUCTION => Some("illegal instruction"),
        STATUS_PRIVILEGED_INSTRUCTION => Some("privileged instruction"),
        STATUS_INTEGER_DIVIDE_BY_ZERO => Some("integer divide by zero"),
        STATUS_STACK_OVERFLOW => Some("stack overflow"),
        STATUS_HEAP_CORRUPTION => Some("heap corruption"),
        STATUS_STACK_BUFFER_OVERRUN => Some("fast-fail (stack buffer overrun)"),
        _ => None,
    }
}

/// Attributes a death to the CLI root or to an intermediate spawner.
pub fn attribution(report: &CrashReport) -> String {
    let parent = report
        .nodes
        .iter()
        .find(|node| node.pid == report.focus_pid)
        .map(|node| node.parent_pid);

    match parent {
        // A top-level process has no tracked parent; the mirrord CLI launched it directly.
        None | Some(0) => "it is a top-level process launched by the mirrord CLI".to_owned(),
        Some(parent) if parent == report.root_pid => {
            format!("its parent is the mirrord CLI root (pid {parent})")
        }
        Some(parent) => {
            let name = report
                .nodes
                .iter()
                .find(|node| node.pid == parent)
                .map(|node| node.name.as_str())
                .unwrap_or("unknown");
            format!("its parent is an intermediate spawner {name}(pid {parent})")
        }
    }
}

/// Composes the short, user-facing report text.
fn report_text(report: &CrashReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "mirrord has hit an unexpected problem!");
    let _ = writeln!(out);

    // A Minecraft-style witty one-liner, freshly shuffled each report. The quote itself is the link
    // to where the tradition comes from, written as markdown: the dialog renders the quote blue and
    // clickable, and the report file keeps the readable `[quote](url)`.
    let witty = WITTY_COMMENTS[rand::random::<u32>() as usize % WITTY_COMMENTS.len()];
    let _ = writeln!(out, "\"[{witty}]({CRASH_WIKI_URL})\"");
    let _ = writeln!(out);

    // Sharing guidance, up top: the bundle can hold secrets; the short report is safe to post.
    let _ = writeln!(
        out,
        "The crash report .zip (and the memory dump inside it) can contain sensitive information: \
         layer logs, environment values, tokens, request and response bodies, file paths, and host \
         names. Please share the full .zip only with a MetalBear employee. This short report, \
         however, is fine to include in public.",
    );
    let _ = writeln!(out);

    // Contact links, near the top, Slack and GitHub first. Each is a markdown link: the dialog
    // renders the label blue and clickable, the report file keeps the readable `[label](url)`.
    let _ = writeln!(
        out,
        ">> Reach out to us directly on [Slack]({METALBEAR_SLACK_URL}) (ask in #mirrord-help)",
    );
    let _ = writeln!(
        out,
        ">> Or file a [bug report on GitHub]({GITHUB_NEW_ISSUE_URL})"
    );
    let _ = writeln!(
        out,
        ">> Or email us at [{METALBEAR_EMAIL}](mailto:{METALBEAR_EMAIL})"
    );
    let _ = writeln!(out);

    match &report.outcome {
        Outcome::Crashed => {
            let _ = writeln!(
                out,
                "A process running under mirrord crashed: {}(pid {}).",
                report.focus_name, report.focus_pid,
            );
        }
        Outcome::InitFailed { reason } => {
            let _ = writeln!(
                out,
                "mirrord's layer failed to initialize in {}(pid {}), so the process did not start \
                 under mirrord.",
                report.focus_name, report.focus_pid,
            );
            let _ = writeln!(out, "Reason: {reason}");
        }
        Outcome::Terminated { exit_code } if is_crash_code(*exit_code) => {
            // The exit code is an NTSTATUS fault, so this was a crash the in-process handler did
            // not capture (no dump), not an external kill.
            let named = exception_name(*exit_code).unwrap_or("a fatal exception");
            let _ = writeln!(
                out,
                "A process running under mirrord crashed: {}(pid {}) — {named} ({exit_code:#010x}). \
                 The in-process handler did not capture it, so no memory dump is available for this \
                 one.",
                report.focus_name, report.focus_pid,
            );
        }
        Outcome::Terminated { exit_code } => {
            let _ = writeln!(
                out,
                "A process running under mirrord was terminated unexpectedly: {}(pid {}), \
                 exit code {exit_code:#010x}. No crash handler fired, which usually means it was \
                 killed from the outside.",
                report.focus_name, report.focus_pid,
            );
            let _ = writeln!(out, "Attribution: {}.", attribution(report));
        }
    }

    let _ = writeln!(out, "mirrord version: {}", report.mirrord_version);
    let _ = writeln!(out);
    let _ = writeln!(out, "Process tree:");
    let _ = write!(out, "{}", format_tree(report));
    let _ = writeln!(out);

    if let Some(record) = report.record {
        let _ = writeln!(out, "Crash record: {}", record.display());
    }

    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Attach the .zip next to this report when you reach out: it bundles this report, the crash \
         record, the module list, the session's layer logs, and the memory dump when one was \
         captured.",
    );

    out
}

/// Writes the report text to its flat file.
fn write_report(directory: &Path, report: &CrashReport, text: &str) {
    let path = directory.join(format!("{}.report.txt", report.stem));
    if let Err(error) = File::create(&path).and_then(|mut file| file.write_all(text.as_bytes())) {
        tracing::warn!(%error, "crash report: failed to write the report text");
    }
}

/// Builds the attachable `.zip`, or returns `None` when it would hold only the report.
///
/// The archive is the one thing to attach: the report, the crash record, the module inventory,
/// every registered session log, and the minidump when dumps are enabled. The dump lives only here
/// — its loose file is removed once archived.
///
/// A single-file zip adds nothing over the loose `.report.txt`, so when there is nothing else to
/// bundle, none is written. Entries are stored uncompressed: no compression time and a minimal
/// dependency surface.
///
/// # Arguments
///
/// * `directory` - the crash directory holding the artifacts.
/// * `report` - the crash facts, including the session logs and the module inventory.
/// * `report_text` - the rendered report, written in as `crash-report.txt`.
///
/// # Returns
///
/// The archive path, or `None` when only the report would be bundled.
fn build_archive(
    directory: &Path,
    report: &CrashReport,
    report_text: &str,
) -> io::Result<Option<PathBuf>> {
    let record = report.record.and_then(|path| std::fs::read(path).ok());
    let logs: Vec<(String, Vec<u8>)> = report
        .logs
        .iter()
        .filter_map(|log| {
            let name = log.file_name()?.to_str()?.to_owned();
            Some((name, std::fs::read(log).ok()?))
        })
        .collect();

    let dump_path = directory.join(format!("{}.dmp", report.stem));
    let has_dump = report.dump.is_some() && dump_path.exists();

    // Nothing but the report — the loose `.report.txt` is enough; skip the pointless one-file zip.
    if record.is_none() && logs.is_empty() && report.modules.is_none() && !has_dump {
        return Ok(None);
    }

    let path = directory.join(format!("{}.zip", report.stem));
    let file = File::create(&path)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let to_io = |error: zip::result::ZipError| io::Error::other(error);

    // The report already embeds the process tree.
    zip.start_file("crash-report.txt", options).map_err(to_io)?;
    zip.write_all(report_text.as_bytes())?;

    if let Some(data) = record {
        zip.start_file("crash-record.txt", options).map_err(to_io)?;
        zip.write_all(&data)?;
    }

    if let Some(modules) = report.modules {
        zip.start_file("modules.txt", options).map_err(to_io)?;
        zip.write_all(modules.as_bytes())?;
    }

    for (name, data) in logs {
        zip.start_file(name, options).map_err(to_io)?;
        zip.write_all(&data)?;
    }

    // The dump goes in last and is streamed, so a full-memory dump never loads into RAM. The loose
    // file is then removed: the dump lives only inside the `.zip`.
    if has_dump && let Ok(mut dump_file) = File::open(&dump_path) {
        zip.start_file("dump.dmp", options).map_err(to_io)?;
        io::copy(&mut dump_file, &mut zip)?;
        drop(dump_file);
        if let Err(error) = std::fs::remove_file(&dump_path) {
            tracing::warn!(%error, "crash report: failed to remove the loose dump after archiving");
        }
    }

    zip.finish().map_err(to_io)?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Representative NTSTATUS codes, by class.
    const ACCESS_VIOLATION: u32 = 0xC000_0005;
    const STACK_OVERFLOW: u32 = 0xC000_00FD;
    const FAIL_FAST: u32 = 0xC000_0409;
    const HEAP_CORRUPTION: u32 = 0xC000_0374;
    const DIVIDE_BY_ZERO: u32 = 0xC000_0094; // a native fault we do not name
    const CTRL_C: u32 = 0xC000_013A;
    const MANAGED_DOTNET: u32 = 0xE043_4352; // customer bit set
    const CPP_EH: u32 = 0xE06D_7363; // customer bit set
    const BREAKPOINT: u32 = 0x8000_0003; // warning severity, not error
    const CLEAN: u32 = 0x0;
    const APP_EXIT: u32 = 0x1;

    /// The single classification table both `is_native_fault` and `is_crash_code` are checked
    /// against, so their one documented divergence (Ctrl-C) is explicit, not prose.
    #[test]
    fn classification_table() {
        // (code, is_native_fault, is_crash_code, is_app_level_exit)
        let cases = [
            (ACCESS_VIOLATION, true, true, false),
            (STACK_OVERFLOW, true, true, false),
            (FAIL_FAST, true, true, false),
            (HEAP_CORRUPTION, true, true, false),
            (DIVIDE_BY_ZERO, true, true, false),
            // Ctrl-C is a native-severity code, but an exit code the runtime owns: the sole point
            // where the in-process gate (`is_native_fault`) and the monitor verdict
            // (`is_crash_code`) diverge.
            (CTRL_C, true, false, true),
            (MANAGED_DOTNET, false, false, true),
            (CPP_EH, false, false, true),
            (BREAKPOINT, false, false, false),
            (CLEAN, false, false, false),
            (APP_EXIT, false, false, false),
        ];
        for (code, native, crash, app) in cases {
            assert_eq!(is_native_fault(code), native, "is_native_fault({code:#x})");
            assert_eq!(is_crash_code(code), crash, "is_crash_code({code:#x})");
            assert_eq!(is_app_level_exit(code), app, "is_app_level_exit({code:#x})");
        }
    }

    fn report<'a>(
        outcome: Outcome,
        nodes: &'a [ProcessNode],
        focus_pid: u32,
        focus_name: &'a str,
        root_pid: u32,
    ) -> CrashReport<'a> {
        CrashReport {
            focus_pid,
            focus_name,
            outcome,
            nodes,
            root_pid,
            dump: None,
            record: None,
            logs: &[],
            modules: None,
            mirrord_version: "test",
            stem: "stem",
        }
    }

    fn node(pid: u32, parent_pid: u32, name: &str) -> ProcessNode {
        ProcessNode {
            pid,
            parent_pid,
            name: name.to_owned(),
            role: "child".to_owned(),
            log_path: None,
            exit_code: None,
        }
    }

    fn dead_node(pid: u32, parent_pid: u32, name: &str, exit_code: u32) -> ProcessNode {
        ProcessNode {
            exit_code: Some(exit_code),
            ..node(pid, parent_pid, name)
        }
    }

    #[test]
    fn presented_as_crash_by_outcome() {
        let crashed = report(Outcome::Crashed, &[], 1, "a.exe", 0);
        assert!(presented_as_crash(&crashed));

        let fault = report(
            Outcome::Terminated {
                exit_code: FAIL_FAST,
            },
            &[],
            1,
            "a.exe",
            0,
        );
        assert!(presented_as_crash(&fault));

        let kill = report(Outcome::Terminated { exit_code: 0 }, &[], 1, "a.exe", 0);
        assert!(!presented_as_crash(&kill));

        let init = report(
            Outcome::InitFailed {
                reason: "x".to_owned(),
            },
            &[],
            1,
            "a.exe",
            0,
        );
        assert!(!presented_as_crash(&init));
    }

    #[test]
    fn dialog_title_by_outcome() {
        let init = report(
            Outcome::InitFailed {
                reason: "x".to_owned(),
            },
            &[],
            1,
            "a.exe",
            0,
        );
        assert_eq!(dialog_title(&init), "mirrord layer failed to start");

        let crashed = report(Outcome::Crashed, &[], 1, "a.exe", 0);
        assert_eq!(dialog_title(&crashed), "mirrord caught a crash");

        let kill = report(Outcome::Terminated { exit_code: 0 }, &[], 1, "a.exe", 0);
        assert_eq!(dialog_title(&kill), "A process was terminated");
    }

    #[test]
    fn focus_mark_by_outcome() {
        let fault = report(
            Outcome::Terminated {
                exit_code: FAIL_FAST,
            },
            &[],
            1,
            "a.exe",
            0,
        );
        assert_eq!(focus_mark(&fault), " ✗ crashed");

        let kill = report(Outcome::Terminated { exit_code: 7 }, &[], 1, "a.exe", 0);
        assert_eq!(focus_mark(&kill), " ✗ terminated");
    }

    #[test]
    fn attribution_cases() {
        // Focus not registered -> top-level.
        let orphan = report(Outcome::Crashed, &[], 99, "a.exe", 10);
        assert!(attribution(&orphan).contains("top-level"));

        // Parent is the CLI root.
        let nodes = [node(50, 10, "a.exe")];
        let root_child = report(Outcome::Crashed, &nodes, 50, "a.exe", 10);
        assert!(attribution(&root_child).contains("mirrord CLI root"));

        // Parent is an intermediate spawner present in the tree.
        let nodes = [node(20, 10, "node.exe"), node(50, 20, "a.exe")];
        let nested = report(Outcome::Crashed, &nodes, 50, "a.exe", 10);
        let text = attribution(&nested);
        assert!(text.contains("intermediate spawner"));
        assert!(text.contains("node.exe"));
    }

    #[test]
    fn format_tree_chain_and_orphan() {
        let nodes = [node(20, 10, "node.exe"), node(50, 20, "crasher.exe")];
        let tree = format_tree(&report(Outcome::Crashed, &nodes, 50, "crasher.exe", 10));
        assert!(tree.contains("mirrord(10)"));
        assert!(tree.contains("node.exe(20)"));
        assert!(tree.contains("crasher.exe(50)"));
        assert!(tree.contains("✗ crashed"));

        // A node whose parent is neither the root nor known is rendered as its own root.
        let nodes = [node(50, 999, "orphan.exe")];
        let tree = format_tree(&report(Outcome::Crashed, &nodes, 10, "orphan.exe", 10));
        assert!(tree.contains("orphan.exe(50)"));
    }

    /// A process that already died this session stays in the tree, marked dead — e.g. a parent that
    /// died before its child could finish initializing (the init-failure race).
    #[test]
    fn format_tree_marks_a_dead_parent() {
        let nodes = [
            dead_node(20, 10, "node.exe", 0xC000_0409),
            node(50, 20, "crasher.exe"),
        ];
        let report = report(
            Outcome::InitFailed {
                reason: "No init event found for pid 50".to_owned(),
            },
            &nodes,
            50,
            "crasher.exe",
            10,
        );
        let tree = format_tree(&report);
        // The dead parent is present and marked; the focused child carries the init-fail mark.
        assert!(tree.contains("node.exe(20) [child] [exited 0xc0000409]"));
        assert!(tree.contains("crasher.exe(50)"));
        assert!(tree.contains("✗ failed to start"));
    }

    /// A top-level process registers `parent_pid` 0; it must hang under the mirrord root with a
    /// connector, not float as its own flat root line.
    #[test]
    fn format_tree_attaches_top_level_under_root() {
        let nodes = [node(20, 0, "crasher.exe"), node(50, 20, "node.exe")];
        let tree = format_tree(&report(Outcome::Crashed, &nodes, 20, "crasher.exe", 10));
        assert!(tree.contains("mirrord(10)"));
        // The connector proves crasher hangs under the root rather than printing flat.
        assert!(tree.contains("└─ crasher.exe(20)"));
        assert!(tree.contains("node.exe(50)"));
    }

    /// Recycled pids can form a parent cycle; the walk must terminate at `MAX_TREE_DEPTH` rather
    /// than overflow the watcher thread's stack.
    #[test]
    fn format_tree_survives_a_cycle() {
        let nodes = [node(20, 30, "a.exe"), node(30, 20, "b.exe")];
        let _ = format_tree(&report(Outcome::Crashed, &nodes, 10, "a.exe", 10));
    }

    #[test]
    fn report_text_includes_record_only_when_present() {
        let nodes = [node(50, 10, "crasher.exe")];
        let base = report(Outcome::Crashed, &nodes, 50, "crasher.exe", 10);
        let text = report_text(&base);
        assert!(!text.contains("Crash record:"));
        // The top matter is always present: a witty `//` line + wiki link, the sharing warning, and
        // the Slack/GitHub contact links.
        assert!(text.contains("// \""));
        assert!(text.contains(CRASH_WIKI_URL));
        assert!(text.contains("sensitive information"));
        assert!(text.contains(METALBEAR_SLACK_URL));
        assert!(text.contains(GITHUB_NEW_ISSUE_URL));

        let record = std::path::PathBuf::from("C:/tmp/incident.record.txt");
        let mut with_record = report(Outcome::Crashed, &nodes, 50, "crasher.exe", 10);
        with_record.record = Some(record.as_path());
        let text = report_text(&with_record);
        assert!(text.contains("Crash record:"));
        assert!(text.contains("incident.record.txt"));
    }

    #[test]
    fn report_text_terminated_with_crash_code_reads_as_a_crash() {
        let nodes = [node(50, 10, "crasher.exe")];
        let text = report_text(&report(
            Outcome::Terminated {
                exit_code: FAIL_FAST,
            },
            &nodes,
            50,
            "crasher.exe",
            10,
        ));
        assert!(text.contains("crashed"));
        assert!(text.contains("did not capture"));
    }
}
