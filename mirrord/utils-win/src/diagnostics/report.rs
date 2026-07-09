//! The crash report.
//!
//! The monitor composes this after a dump or a detected death — the safe place for it; the crashing
//! process never builds a report.
//!
//! Each incident writes a set of flat files in the crash directory, all sharing the same stem so
//! they cluster together:
//!
//! - `<stem>.report.txt` — the short, non-scary summary the user reads and copies. It is the source
//!   of truth, logged to the console and shown in the native [`dialog`](super::dialog).
//! - `<stem>.record.txt` — the raw in-process crash record (code, address, faulting module, stack),
//!   when the handler captured one.
//! - `<stem>.modules.txt` — the crashing process's module inventory, when it could be enumerated.
//! - `<stem>.dmp` — the minidump, when dumps are enabled. It stays on disk for the whole session
//!   and is removed once folded into the archive at session end.
//!
//! Every reportable incident in a session is written to disk; nothing is coalesced away. The one
//! file to attach is a single per-session archive, `mirrord-crash-session_<stamp>.zip`, rebuilt as
//! each incident lands and finalized when the session ends. It bundles every incident's report,
//! record, module list, and minidump, plus the session's layer logs (deduplicated). The minidumps
//! live only there — their loose files are removed once the session is over.
//!
//! The report text and the console output are always written, so a dismissed dialog loses nothing.
//! The dialog is shown once per session (suppressed under `CI`); it blocks until the user dismisses
//! it, and the monitor's watchdog keeps the monitor alive until then.

use std::{
    fmt::Write as _,
    fs::File,
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
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

/// The NTSTATUS customer / application-defined bit (bit 29), `0x2000_0000`.
///
/// A 32-bit NTSTATUS is not opaque: it packs a severity, a customer flag, a facility, and a code.
/// We only read bit 29, but naming the whole layout is what turns "OS fault vs runtime exception"
/// into a one-line test instead of an enumeration of every runtime's magic number.
///
/// ```text
///    31 30 29  28  27            16 15                     0
///   ┌─────┬───┬───┬────────────────┬────────────────────────┐
///   │ Sev │ C │ N │    Facility    │          Code          │
///   └──┬──┴─┬─┴─┬─┴───────┬────────┴───────────┬────────────┘
///      │    │   │         │                    └─ bits 15..0:  the status code proper
///      │    │   │         └─ bits 27..16: facility — which subsystem raised it
///      │    │   └─ bit 28: reserved, always 0
///      │    └─ bit 29: customer / application-defined  ← 0x2000_0000, the bit we test
///      └─ bits 31..30: severity (00 success, 01 info, 10 warning, 11 error)
/// ```
///
/// The OS leaves bit 29 clear on its own faults — access violation `0xC0000005`, stack overflow
/// `0xC00000FD`, divide-by-zero `0xC0000094` — whose top nibble `C` = `1100b` is severity 11 with
/// the customer bit off. A runtime that mints its own codes sets it: a .NET exception
/// (`0xE0434352`) and a C++ EH throw (`0xE06D7363`) both start with nibble `E` = `1110b` (severity
/// 11, customer 1), so both read as application-level and are not treated as mirrord crashes.
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

/// One reported incident in a session, accumulated by the monitor so the session archive can be
/// rebuilt to include every crash.
///
/// The heavy artifacts (record, module list, dump) are read back from the incident's loose files by
/// [`stem`](Self::stem) at build time, so this stays small enough to clone on every rebuild.
#[derive(Clone)]
pub struct Incident {
    /// The process image name.
    pub name: String,
    /// The process id.
    pub pid: u32,
    /// The incident stem its loose artifact files are named from.
    pub stem: String,
}

/// Serializes session-archive rebuilds. Several watcher threads can report at once; the archive is
/// a single file, so its writers take turns.
static ARCHIVE_BUILD: Mutex<()> = Mutex::new(());

/// The largest incident count any rebuild has committed. The incident list only grows, so a rebuild
/// from an older, smaller snapshot must not overwrite a newer one — that would regress the archive
/// the dialog points at.
static ARCHIVE_HWM: AtomicUsize = AtomicUsize::new(0);

/// Set once the session archive is finalized and the loose dumps are gone. Later rebuilds become
/// no-ops, so a straggler crash can neither truncate the finalized archive nor drop the removed
/// dumps.
static ARCHIVE_FINALIZED: AtomicBool = AtomicBool::new(false);

/// Composes the report, writes its flat artifacts, (re)builds the session archive, and shows the
/// dialog.
///
/// This incident's report text — and its module inventory, when there is one — are written as loose
/// files, then the single per-session archive is rebuilt to include every incident so far. The
/// rebuild happens before the dialog so a current, complete bundle already exists when the dialog
/// points the user at it.
///
/// # Arguments
///
/// * `report` - the crash facts.
/// * `directory` - where to write the artifacts.
/// * `show_dialog` - whether this caller claimed the single per-session dialog slot. The caller
///   does the claiming so this function never has to hold a shared lock across the blocking dialog.
/// * `incidents` - every incident reported this session, including this one, for the session
///   archive.
/// * `session_id` - the session stamp the archive file name is built from.
pub fn surface(
    report: &CrashReport,
    directory: &Path,
    show_dialog: bool,
    incidents: &[Incident],
    session_id: &str,
) {
    let report_text = report_text(report);

    // This incident's loose files are the source of truth: the report text always, and the module
    // inventory when we have one. The record is written by the layer, the dump by the monitor.
    write_report(directory, report, &report_text);
    if let Some(modules) = report.modules {
        write_modules(directory, report.stem, modules);
    }

    // Log it so it appears in the monitor's console too.
    tracing::warn!("{report_text}");

    // Rebuild the single session archive from every incident's loose files, so the attachable is
    // current before the dialog points the user at it.
    let session_archive = SessionArchive {
        directory,
        incidents,
        logs: report.logs,
        session_id,
    };
    let archive = build_session_archive(&session_archive)
        .inspect_err(|error| tracing::warn!(%error, "crash report: failed to build the archive"))
        .ok()
        .flatten();
    if let Some(archive) = &archive {
        tracing::info!(path = %archive.display(), "crash report: wrote the attachable archive");
    }

    // Show the dialog whenever this caller claimed the session's single slot — unless we're in CI,
    // where nobody could dismiss it and the monitor would block. It blocks until the user dismisses
    // it, and the monitor's watchdog keeps the monitor alive until then. The files and the console
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
        "The crash report .zip (and the memory dumps inside it) can contain sensitive information: \
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
        "Attach the session .zip next to this report when you reach out — one file collects every \
         crash in this run: each report, crash record, and memory dump, plus the session's layer \
         logs. Look for mirrord-crash-session_*.zip in the crash directory.",
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

/// Writes the module inventory to this incident's flat file, so the session archive can read it
/// back on any rebuild without holding it in memory.
fn write_modules(directory: &Path, stem: &str, modules: &str) {
    let path = directory.join(format!("{stem}.modules.txt"));
    if let Err(error) = File::create(&path).and_then(|mut file| file.write_all(modules.as_bytes()))
    {
        tracing::warn!(%error, "crash report: failed to write the module inventory");
    }
}

/// The inputs a session archive is built from — grouped because the incremental build and the
/// finalize pass take the same set.
pub struct SessionArchive<'a> {
    /// The crash directory holding the loose artifacts and the archive.
    pub directory: &'a Path,
    /// Every incident reported this session.
    pub incidents: &'a [Incident],
    /// The registered session log files to bundle, deduplicated by file name.
    pub logs: &'a [PathBuf],
    /// The session stamp the archive file name is built from.
    pub session_id: &'a str,
}

/// (Re)builds the single per-session attachable archive from every incident's loose files.
///
/// The archive is the one thing to attach: for each incident its report, crash record, module
/// inventory, and minidump; plus every registered session log, deduplicated. It is rebuilt (into a
/// temp file, then atomically renamed) as each incident lands — so a current, complete bundle
/// always exists when the dialog points at it — and
/// once more when the session ends ([`finalize_session_archive`]).
///
/// The heavy per-incident files are read from disk by stem, and the minidump is streamed, so a
/// full-memory dump never loads into RAM. The loose dumps are left in place for the next rebuild;
/// finalizing removes them once the session is over. Entries are stored uncompressed: no
/// compression time and a minimal dependency surface.
///
/// # Arguments
///
/// * `archive` - the session archive inputs (directory, incidents, session logs, session id).
///
/// # Returns
///
/// The archive path, or `None` when there is nothing but reports to bundle.
fn build_session_archive(archive: &SessionArchive) -> io::Result<Option<PathBuf>> {
    let &SessionArchive {
        directory,
        incidents,
        logs,
        session_id,
    } = archive;
    if incidents.is_empty() {
        return Ok(None);
    }

    let path = directory.join(format!("mirrord-crash-session_{session_id}.zip"));

    // One writer at a time: the archive is a single file rebuilt from several watcher threads.
    let _guard = ARCHIVE_BUILD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // Once finalized (loose dumps removed) a rebuild would only truncate the archive and drop those
    // dumps; and since the incident list only grows, an older, smaller snapshot must never
    // overwrite a newer one — that would regress the bundle the dialog points at. Skip both,
    // keeping the file.
    if ARCHIVE_FINALIZED.load(Ordering::SeqCst)
        || incidents.len() < ARCHIVE_HWM.load(Ordering::SeqCst)
    {
        return Ok(path.exists().then_some(path));
    }

    // The caller passes the whole session's log set as one flat list; two registrations can name
    // the same file, so keep the first sighting of each file name.
    let mut seen = std::collections::HashSet::new();
    let logs: Vec<(String, PathBuf)> = logs
        .iter()
        .filter_map(|log| {
            let name = log.file_name()?.to_str()?.to_owned();
            seen.insert(name.clone()).then_some((name, log.clone()))
        })
        .collect();

    // A report-only session (no records, dumps, or logs) adds nothing over the loose
    // `.report.txt`s.
    let has_payload = !logs.is_empty()
        || incidents.iter().any(|incident| {
            ["record.txt", "modules.txt", "dmp"].iter().any(|suffix| {
                directory
                    .join(format!("{}.{suffix}", incident.stem))
                    .exists()
            })
        });
    if !has_payload {
        return Ok(None);
    }

    // Build into a temp file and atomically rename over the target, so a failed build never
    // destroys the last-good archive and no reader ever observes a half-written zip.
    let temp = directory.join(format!("mirrord-crash-session_{session_id}.zip.tmp"));
    let file = File::create(&temp)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let to_io = |error: zip::result::ZipError| io::Error::other(error);

    // The focus (first) incident's report at the root, for an at-a-glance summary; it already
    // embeds the full process tree.
    if let Some(first) = incidents.first()
        && let Ok(text) = std::fs::read(directory.join(format!("{}.report.txt", first.stem)))
    {
        zip.start_file("crash-report.txt", options).map_err(to_io)?;
        zip.write_all(&text)?;
    }

    // Each incident's artifacts under its own folder, keyed by the stem — it carries the timestamp,
    // so the folder stays unique even if a pid is recycled to another same-named process.
    for incident in incidents {
        let dir = format!("incidents/{}", incident.stem);
        for (suffix, entry) in [
            ("report.txt", "crash-report.txt"),
            ("record.txt", "crash-record.txt"),
            ("modules.txt", "modules.txt"),
        ] {
            let src = directory.join(format!("{}.{suffix}", incident.stem));
            if let Ok(data) = std::fs::read(&src) {
                zip.start_file(format!("{dir}/{entry}"), options)
                    .map_err(to_io)?;
                zip.write_all(&data)?;
            }
        }

        // The dump is streamed rather than read into memory; its loose file stays for the next
        // rebuild and is removed only when the session is finalized.
        let dump_path = directory.join(format!("{}.dmp", incident.stem));
        if dump_path.exists()
            && let Ok(mut dump_file) = File::open(&dump_path)
        {
            zip.start_file(format!("{dir}/dump.dmp"), options)
                .map_err(to_io)?;
            io::copy(&mut dump_file, &mut zip)?;
        }
    }

    // The session logs, once each.
    for (name, log) in logs {
        if let Ok(data) = std::fs::read(&log) {
            zip.start_file(format!("layer-logs/{name}"), options)
                .map_err(to_io)?;
            zip.write_all(&data)?;
        }
    }

    zip.finish().map_err(to_io)?;
    std::fs::rename(&temp, &path)?;
    ARCHIVE_HWM.store(incidents.len(), Ordering::SeqCst);
    Ok(Some(path))
}

/// Rebuilds the session archive a final time and removes the loose minidumps.
///
/// Called once, when the session ends. The last per-incident rebuild already holds everything, but
/// a final pass covers any incident whose own rebuild failed; only then are the loose dumps
/// removed, so a dump ends up living only inside the archive.
///
/// # Arguments
///
/// * `archive` - the session archive inputs (directory, incidents, session logs, session id).
pub fn finalize_session_archive(archive: &SessionArchive) {
    let &SessionArchive {
        directory,
        incidents,
        ..
    } = archive;
    if incidents.is_empty() {
        return;
    }
    if let Err(error) = build_session_archive(archive) {
        tracing::warn!(%error, "crash report: failed to finalize the session archive");
        return;
    }
    for incident in incidents {
        let dump = directory.join(format!("{}.dmp", incident.stem));
        if dump.exists()
            && let Err(error) = std::fs::remove_file(&dump)
        {
            tracing::warn!(%error, "crash report: failed to remove the loose dump after archiving");
        }
    }
    ARCHIVE_FINALIZED.store(true, Ordering::SeqCst);
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

    #[test]
    fn session_archive_bundles_every_incident_and_dedups_logs() {
        use std::io::Read as _;

        // A unique scratch dir; the pid keeps concurrent test binaries from colliding.
        let dir =
            std::env::temp_dir().join(format!("mirrord-session-archive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Two incidents' loose files plus one session log both of them carry.
        let write = |name: &str, data: &str| std::fs::write(dir.join(name), data).unwrap();
        write("mirrord-crash_a_crasher_pid100.report.txt", "parent report");
        write("mirrord-crash_a_crasher_pid100.record.txt", "parent record");
        write("mirrord-crash_a_crasher_pid100.dmp", "PARENTDUMP");
        write("mirrord-crash_b_node_pid200.report.txt", "child report");
        let log = dir.join("mirrord-layer_shared.log");
        std::fs::write(&log, "shared log").unwrap();

        let incidents = [
            Incident {
                name: "crasher".to_owned(),
                pid: 100,
                stem: "mirrord-crash_a_crasher_pid100".to_owned(),
            },
            Incident {
                name: "node".to_owned(),
                pid: 200,
                stem: "mirrord-crash_b_node_pid200".to_owned(),
            },
        ];
        // Both incidents carry the same session-wide log set: it must land once.
        let logs = [log.clone(), log.clone()];

        let path = build_session_archive(&SessionArchive {
            directory: &dir,
            incidents: &incidents,
            logs: &logs,
            session_id: "sess",
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "mirrord-crash-session_sess.zip",
        );

        let mut archive = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let names: Vec<String> = archive.file_names().map(str::to_owned).collect();

        // Root focus report, every incident's own folder, and the shared log exactly once.
        assert!(names.iter().any(|n| n == "crash-report.txt"));
        assert!(
            names
                .iter()
                .any(|n| n == "incidents/mirrord-crash_a_crasher_pid100/crash-report.txt")
        );
        assert!(
            names
                .iter()
                .any(|n| n == "incidents/mirrord-crash_a_crasher_pid100/crash-record.txt")
        );
        assert!(
            names
                .iter()
                .any(|n| n == "incidents/mirrord-crash_a_crasher_pid100/dump.dmp")
        );
        assert!(
            names
                .iter()
                .any(|n| n == "incidents/mirrord-crash_b_node_pid200/crash-report.txt")
        );
        let log_entries = names
            .iter()
            .filter(|n| n.starts_with("layer-logs/"))
            .count();
        assert_eq!(log_entries, 1, "the shared layer log must be deduplicated");

        // The dump content round-trips through the archive.
        let mut dump = archive
            .by_name("incidents/mirrord-crash_a_crasher_pid100/dump.dmp")
            .unwrap();
        let mut buf = String::new();
        dump.read_to_string(&mut buf).unwrap();
        assert_eq!(buf, "PARENTDUMP");
        drop(dump);
        drop(archive);

        let _ = std::fs::remove_dir_all(&dir);
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
