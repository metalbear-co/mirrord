//! `mirrord diagnose timings`: where a session's time went, in two sections.
//!
//! "Session start" is what the operator did before the application could start. The operator
//! keeps it in memory and the command asks for it through the operator's API.
//!
//! "Application calls" is what the application then waited on mirrord for. The layer counts
//! every call it answers from the cluster and the session's internal proxy keeps the totals in
//! `~/.mirrord/timings/<session id>.json` (see [`mirrord_intproxy::timings`]). That part is read
//! from the file, so it works while the session runs, after it is gone, and without a cluster.

use std::{
    cmp::Reverse,
    env::home_dir,
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use chrono::{DateTime, Utc};
use mirrord_analytics::NullReporter;
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_intproxy::timings::{
    DirLookups, KindSummary, SUMMARY_EXTENSION, SessionLabel, SlowCallSummary, TimingsSummary,
};
use mirrord_operator::client::{
    OperatorApi,
    session_start::{SessionStartTimings, StartConnect, StartPhase},
};
use mirrord_progress::NullProgress;
use prettytable::{
    Table,
    format::consts::{FORMAT_CLEAN, FORMAT_NO_LINESEP_WITH_TITLE},
    row,
};
use serde::Serialize;

use crate::{
    CliError, CliResult,
    util::{remove_proxy_env, resolve_layer_config},
};

/// The most suggestions printed under the table. A person acts on the first few and skims past
/// the rest, so the list stays short and holds the ones that cost the most time.
const MAX_SUGGESTIONS: usize = 3;

/// How many lookups a directory needs before it is worth a suggestion. Below this, even a slow
/// cluster makes the saving too small to be worth a config change.
const MIN_LOOKUPS_FOR_SUGGESTION: u64 = 20;

/// Where the internal proxy writes every session's summary, or [`None`] when the user has no
/// home directory.
pub(crate) fn timings_dir() -> Option<PathBuf> {
    home_dir().map(|home_dir| home_dir.join(".mirrord").join("timings"))
}

/// The summary file of `session` in `dir`, or the one modified last when no session is named,
/// with its modification time.
///
/// # Returns
///
/// [`None`] when there is no such file. A named session is looked up by its file name, so a name
/// that could point outside `dir` matches nothing.
fn summary_file(dir: &Path, session: Option<&str>) -> io::Result<Option<(PathBuf, SystemTime)>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };

    let mut found = None;
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != SUMMARY_EXTENSION) {
            continue;
        }
        // Operator session ids are hex, which people type in either case.
        if let Some(session) = session
            && path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_none_or(|stem| !stem.eq_ignore_ascii_case(session))
        {
            continue;
        }
        // The file's session may be removing it right now to make room for a newer one.
        let Ok(modified) = fs::metadata(&path).and_then(|metadata| metadata.modified()) else {
            continue;
        };
        if found.as_ref().is_none_or(|(_, newest)| modified > *newest) {
            found = Some((path, modified));
        }
    }

    Ok(found)
}

/// A wait as a person reads it: the unit follows the size, with about three digits.
fn format_wait(nanos: u64) -> String {
    const MICRO: u64 = 1_000;
    const MILLI: u64 = 1_000_000;
    const SECOND: u64 = 1_000_000_000;

    match nanos {
        0..MICRO => format!("{nanos} ns"),
        MICRO..MILLI => format!("{:.0} µs", nanos as f64 / MICRO as f64),
        MILLI..SECOND => format!("{:.1} ms", nanos as f64 / MILLI as f64),
        _ => format!("{:.2} s", nanos as f64 / SECOND as f64),
    }
}

fn is_under(dir: &str, root: &str) -> bool {
    dir.strip_prefix(root)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// The directories where every path lookup that went to the cluster came back "not found", the
/// ones that took the most time first.
///
/// Answering "not found" locally for such a directory gives the application exactly what it got
/// in this session, without the round trip, so pointing at it is right whatever the application
/// is. A directory qualifies only when nothing in it or below it was ever found, and the numbers
/// of every directory below it are folded into it so one tree gets one suggestion.
///
/// No directory qualifies when the summary lost track of some lookups: one of them may have
/// found a file below a directory that looks empty here.
fn not_found_dirs(summary: &TimingsSummary) -> Vec<DirLookups> {
    if summary.untracked_path_lookups > 0 {
        return Vec::new();
    }

    let mut dirs = summary
        .path_lookups
        .iter()
        // A pattern for the root would cover the whole file system.
        .filter(|root| root.dir != "/")
        .map(|root| {
            summary
                .path_lookups
                .iter()
                .filter(|dir| dir.dir == root.dir || is_under(&dir.dir, &root.dir))
                .fold(
                    DirLookups {
                        dir: root.dir.clone(),
                        lookups: 0,
                        not_found: 0,
                        total_nanos: 0,
                    },
                    |mut tree, dir| {
                        tree.lookups = tree.lookups.saturating_add(dir.lookups);
                        tree.not_found = tree.not_found.saturating_add(dir.not_found);
                        tree.total_nanos = tree.total_nanos.saturating_add(dir.total_nanos);
                        tree
                    },
                )
        })
        .filter(|tree| tree.not_found == tree.lookups && tree.lookups >= MIN_LOOKUPS_FOR_SUGGESTION)
        .collect::<Vec<_>>();

    let roots = dirs.iter().map(|tree| tree.dir.clone()).collect::<Vec<_>>();
    dirs.retain(|tree| !roots.iter().any(|root| is_under(&tree.dir, root)));
    dirs.sort_by(|a, b| {
        b.total_nanos
            .cmp(&a.total_nanos)
            .then_with(|| a.dir.cmp(&b.dir))
    });
    dirs.truncate(MAX_SUGGESTIONS);

    dirs
}

/// `text` with its control characters written out as escapes.
///
/// Paths, host names, the summary file's fields and the operator's words come from outside this
/// command, and one with a terminal control sequence in it would otherwise act on the terminal
/// of whoever reads the text report. The JSON report escapes them by being JSON.
fn printable(text: &str) -> String {
    let mut printable = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_control() {
            printable.extend(character.escape_default());
        } else {
            printable.push(character);
        }
    }
    printable
}

/// What the user can change, for the cases the numbers alone settle. Empty when no rule is
/// certain, which is the usual case.
fn suggestions(summary: &TimingsSummary) -> Vec<String> {
    not_found_dirs(summary)
        .into_iter()
        .map(|tree| {
            // The pattern is printed as a JSON string, ready to paste into a `mirrord.json`.
            // `regex::escape` also escapes `-`, which only matters inside a character class and
            // would need a doubled backslash in JSON for nothing.
            // File patterns ignore case, and `(?-i)` turns that off for this one: the session
            // says nothing about the same path in another case.
            let pattern = format!("(?-i)^{}/", regex::escape(&tree.dir).replace(r"\-", "-"));
            format!(
                "{lookups} file lookups under `{dir}` went to the cluster (about {wait}) and \
                every one came back \"not found\". Add {pattern} to `feature.fs.not_found` to \
                give the same answer without the round trip, or to `feature.fs.local` if those \
                files are on your machine.",
                lookups = tree.lookups,
                dir = printable(&tree.dir),
                wait = format_wait(tree.total_nanos),
                pattern = serde_json::Value::from(pattern),
            )
        })
        .collect()
}

/// How long the command waits for the operator's answer about a session's start.
///
/// The application's numbers are already on this machine, so an unreachable cluster or a login
/// prompt nobody answers must not keep them from being printed. Twenty seconds covers a slow
/// cluster login; past it the operator's section says it could not be asked.
const OPERATOR_ANSWER_TIMEOUT: Duration = Duration::from_secs(20);

/// Headings of the two sections. Scripts and tests of other repositories match on them, so they
/// stay as they are.
const SESSION_START_HEADING: &str = "Session start";
const APPLICATION_HEADING: &str = "Application calls";

/// The operator's part of the report: what it did before the application could start.
///
/// The phase names are the operator's own and are printed as they come, also the ones this
/// version does not know, because scripts match on them.
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum SessionStart {
    Recorded {
        connects: Vec<StartConnect>,
        /// On a session that runs on several clusters: the clusters that had nothing to report.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        missing_clusters: Vec<String>,
    },
    /// The session did not go through an operator, so nobody timed its start.
    NoOperator,
    /// The operator predates start timings.
    Unsupported,
    /// The operator has nothing for this session.
    Forgotten,
    /// The operator could not be asked.
    Unavailable { reason: String },
}

/// The application's part of the report: the summary file's numbers plus what this command adds.
#[derive(Serialize)]
struct Application {
    /// RFC 3339 time the summary was last written.
    updated_at: String,
    #[serde(skip)]
    updated: SystemTime,
    kinds: Vec<KindSummary>,
    slowest_calls: Vec<SlowCallSummary>,
    path_lookups: Vec<DirLookups>,
    untracked_path_lookups: u64,
    suggestions: Vec<String>,
}

/// Everything the command prints. As JSON it is one object with a key per section.
#[derive(Serialize)]
struct Report {
    session_id: String,
    /// What the session's summary file says about it. `null` when this machine has no file for
    /// the session, which then has no `application` section either.
    session: Option<SessionLabel>,
    session_start: SessionStart,
    application: Option<Application>,
}

fn millis_as_nanos(millis: u64) -> u64 {
    millis.saturating_mul(1_000_000)
}

/// A duration the operator measured in milliseconds, which has no finer unit to show.
fn format_millis(millis: u64) -> String {
    if millis == 0 {
        "0 ms".to_owned()
    } else {
        format_wait(millis_as_nanos(millis))
    }
}

/// A point on the session's timeline, counted from its first step.
fn format_offset(millis: u64) -> String {
    format!("+{}", format_millis(millis))
}

fn render_session_start(session_start: &SessionStart, output: &mut String) {
    let connects = match session_start {
        // A session without an operator has no start to report, and saying so every time would
        // be noise for users who never use one.
        SessionStart::NoOperator => return,
        SessionStart::Recorded {
            connects,
            missing_clusters,
        } if connects.iter().any(|connect| connect.cluster.is_some()) => {
            render_multi_cluster_start(connects, missing_clusters, output);
            return;
        }
        SessionStart::Recorded { connects, .. } => connects,
        SessionStart::Unsupported => {
            output.push_str(&format!(
                "\n{SESSION_START_HEADING}\nThis operator does not record session start timings.\n"
            ));
            return;
        }
        SessionStart::Forgotten => {
            output.push_str(&format!(
                "\n{SESSION_START_HEADING}\nThe operator has no start timings for this session. \
                It keeps them in memory for recent sessions, and loses them when it restarts.\n"
            ));
            return;
        }
        SessionStart::Unavailable { reason } => {
            output.push_str(&format!(
                "\n{SESSION_START_HEADING}\nCould not ask the operator: {}\n",
                printable(reason)
            ));
            return;
        }
    };

    output.push_str(&format!(
        "\n{SESSION_START_HEADING}\nWhat the operator did before your application could start. \
        A session connects more than once while it starts, and connect 1 is the first.\n\n"
    ));

    let mut table = Table::new();
    table.set_format(*FORMAT_NO_LINESEP_WITH_TITLE);
    table.set_titles(row!["Connect", "Phase", "What it is", r->"Duration"]);
    let mut total_millis = 0u64;
    for (number, connect) in (1u32..).zip(connects) {
        if connect.phases.is_empty() {
            table.add_row(row![number, "", "no phase finished yet", ""]);
        }
        for phase in &connect.phases {
            total_millis = total_millis.saturating_add(phase.duration_ms);
            let duration = format_millis(phase.duration_ms);
            table.add_row(row![
                number,
                printable(&phase.name),
                printable(&phase.description),
                r->if phase.failed {
                    format!("{duration} (did not finish)")
                } else {
                    duration
                },
            ]);
        }
    }
    table.add_row(row!["all", "", "", r->format_millis(total_millis)]);
    output.push_str(&table.to_string());
}

/// The part of a multi-cluster session start that one cluster's operator did.
struct ClusterSpan<'a> {
    cluster: &'a str,
    started: u64,
    finished: u64,
    slowest: &'a StartPhase,
}

/// One phase of a multi-cluster session start, placed on the session's timeline.
struct TimedPhase<'a> {
    cluster: &'a str,
    phase: &'a StartPhase,
    /// When the phase started, counted from the start of the session's first connect.
    start_ms: u64,
    end_ms: u64,
}

/// The phases of every cluster on one timeline, the earliest first.
///
/// A phase is placed by its connect's start plus [`StartPhase::started_after_ms`]. An operator
/// that sends no offset gets its phases placed one after the other, which is how a connect runs
/// them. The clusters' clocks are taken to agree, as the clocks of hosts that sync their time do
/// to within milliseconds.
fn timeline(connects: &[StartConnect]) -> Vec<TimedPhase<'_>> {
    let starts = connects
        .iter()
        .map(|connect| connect.started_at.parse::<DateTime<Utc>>().ok())
        .collect::<Vec<_>>();
    let Some(first) = starts.iter().flatten().min().copied() else {
        return Vec::new();
    };

    let mut phases = Vec::new();
    for (connect, start) in connects.iter().zip(starts) {
        let connect_start = start
            .and_then(|start| u64::try_from((start - first).num_milliseconds()).ok())
            .unwrap_or_default();
        let mut next_ms = connect_start;
        for phase in &connect.phases {
            let start_ms = phase
                .started_after_ms
                .map_or(next_ms, |offset| connect_start.saturating_add(offset));
            let end_ms = start_ms.saturating_add(phase.duration_ms);
            next_ms = end_ms;
            phases.push(TimedPhase {
                cluster: connect.cluster.as_deref().unwrap_or("?"),
                phase,
                start_ms,
                end_ms,
            });
        }
    }
    phases.sort_by_key(|timed| timed.start_ms);

    phases
}

/// The start of a session that runs on several clusters: every step on one timeline, a table per
/// cluster, and a line that says which cluster held the start up and why.
fn render_multi_cluster_start(
    connects: &[StartConnect],
    missing_clusters: &[String],
    output: &mut String,
) {
    let phases = timeline(connects);
    // The cluster of the first connect is the one the session was started against.
    let primary = phases
        .first()
        .map(|timed| timed.cluster)
        .unwrap_or_default();
    let mut clusters = Vec::<&str>::new();
    for timed in &phases {
        if !clusters.contains(&timed.cluster) {
            clusters.push(timed.cluster);
        }
    }

    output.push_str(&format!(
        "\n{SESSION_START_HEADING}\nWhat the operators did before your application could start, \
        on {} clusters. At is when a step started, counted from the session's first step.\n\n",
        clusters.len() + missing_clusters.len()
    ));

    let mut table = Table::new();
    table.set_format(*FORMAT_NO_LINESEP_WITH_TITLE);
    table.set_titles(row![r->"At", r->"Took", "Cluster", "Phase", "What it is"]);
    for timed in &phases {
        let took = format_millis(timed.phase.duration_ms);
        // The other clusters' steps are indented: they happen inside the primary's setup.
        let cluster = if timed.cluster == primary {
            printable(timed.cluster)
        } else {
            format!("  {}", printable(timed.cluster))
        };
        table.add_row(row![
            r->format_offset(timed.start_ms),
            r->if timed.phase.failed {
                format!("{took} (did not finish)")
            } else {
                took
            },
            cluster,
            printable(&timed.phase.name),
            printable(&timed.phase.description),
        ]);
    }
    output.push_str(&table.to_string());

    let spans = clusters
        .iter()
        .filter_map(|cluster| {
            let own = phases.iter().filter(|timed| timed.cluster == *cluster);
            let slowest = own
                .clone()
                .max_by_key(|timed| timed.phase.duration_ms)?
                .phase;
            Some(ClusterSpan {
                cluster,
                started: own.clone().map(|timed| timed.start_ms).min()?,
                finished: own.map(|timed| timed.end_ms).max()?,
                slowest,
            })
        })
        .collect::<Vec<_>>();

    output.push_str("\nBy cluster\n");
    let mut table = Table::new();
    table.set_format(*FORMAT_NO_LINESEP_WITH_TITLE);
    table.set_titles(row![
        "Cluster",
        r->"Started",
        r->"Finished",
        r->"Took",
        "Slowest step"
    ]);
    for span in &spans {
        table.add_row(row![
            printable(span.cluster),
            r->format_offset(span.started),
            r->format_offset(span.finished),
            r->format_millis(span.finished.saturating_sub(span.started)),
            format!(
                "{} {}",
                printable(&span.slowest.name),
                format_millis(span.slowest.duration_ms)
            ),
        ]);
    }
    output.push_str(&table.to_string());

    let total = spans
        .iter()
        .map(|span| span.finished)
        .max()
        .unwrap_or_default();
    let mut by_finish = spans.iter().collect::<Vec<_>>();
    by_finish.sort_by_key(|span| Reverse(span.finished));
    // The primary does its own steps first and then connects to the other clusters, so the one
    // of the others that finished last is the one the session waited for.
    let others = by_finish
        .iter()
        .filter(|span| span.cluster != primary)
        .collect::<Vec<_>>();
    output.push_str(&format!("\nThe start took {}.", format_millis(total)));
    match others.as_slice() {
        [last, next, ..] => output.push_str(&format!(
            " {} finished last, {} after {}: its slowest step was {} ({}).",
            printable(last.cluster),
            format_millis(last.finished.saturating_sub(next.finished)),
            printable(next.cluster),
            printable(&last.slowest.name),
            format_millis(last.slowest.duration_ms),
        )),
        [only] => output.push_str(&format!(
            " Its slowest step on {} was {} ({}).",
            printable(only.cluster),
            printable(&only.slowest.name),
            format_millis(only.slowest.duration_ms),
        )),
        [] => {}
    }
    output.push('\n');

    if !missing_clusters.is_empty() {
        output.push_str(&format!(
            "No timings from {}: the operator there records none for this session, or could not \
            be asked.\n",
            missing_clusters
                .iter()
                .map(String::as_str)
                .map(printable)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
}

/// What a slow call was for. A file call without a path worked on a file the application had
/// already opened; the other kinds without a subject have nothing more to say.
fn slow_call_subject(call: &SlowCallSummary) -> &str {
    match call.subject.as_deref() {
        Some(subject) => subject,
        None if call.kind.starts_with("file_") || call.kind == "directory_listing" => {
            "(an open file)"
        }
        None => "",
    }
}

fn render_application(application: Option<&Application>, now: SystemTime, output: &mut String) {
    output.push_str(&format!("\n{APPLICATION_HEADING}\n"));
    let Some(application) = application else {
        output.push_str("This machine has no record of the session's application.\n");
        return;
    };

    let age = now.duration_since(application.updated).unwrap_or_default();
    output.push_str(&format!(
        "What your application waited on mirrord for. Updated {} ({} ago).\n\n",
        application.updated_at,
        humantime::format_duration(Duration::from_secs(age.as_secs())),
    ));

    if application.kinds.is_empty() {
        output.push_str("The application made no calls that mirrord handled for it.\n");
        return;
    }

    let mut table = Table::new();
    table.set_format(*FORMAT_NO_LINESEP_WITH_TITLE);
    table.set_titles(row!["Kind", r->"Calls", r->"Total", r->"Slowest"]);
    for kind in &application.kinds {
        table.add_row(row![
            printable(&kind.kind.replace('_', " ")),
            r->kind.calls,
            r->format_wait(kind.total_nanos),
            r->format_wait(kind.slowest_nanos),
        ]);
    }
    let calls = application
        .kinds
        .iter()
        .fold(0u64, |calls, kind| calls.saturating_add(kind.calls));
    let total = application
        .kinds
        .iter()
        .fold(0u64, |total, kind| total.saturating_add(kind.total_nanos));
    let slowest = application
        .kinds
        .iter()
        .map(|kind| kind.slowest_nanos)
        .max()
        .unwrap_or_default();
    table.add_row(row![
        "all",
        r->calls,
        r->format_wait(total),
        r->format_wait(slowest),
    ]);
    output.push_str(&table.to_string());
    output.push_str(
        "\nTotal is the time the application spent waiting for mirrord's answers. Calls made \
        from several threads at once overlap, so it can be more than the time that passed. \
        Time spent sending and receiving data over connections is not included.\n",
    );

    if !application.slowest_calls.is_empty() {
        output.push_str("\nSlowest calls:\n");
        let mut table = Table::new();
        table.set_format(*FORMAT_CLEAN);
        for call in &application.slowest_calls {
            table.add_row(row![
                r->format_wait(call.nanos),
                printable(&call.kind.replace('_', " ")),
                printable(slow_call_subject(call)),
            ]);
        }
        output.push_str(&table.to_string());
    }

    if !application.suggestions.is_empty() {
        output.push_str("\nSuggestions:\n");
        for suggestion in &application.suggestions {
            output.push_str(&format!("- {suggestion}\n"));
        }
    }
}

fn render(report: &Report, now: SystemTime) -> String {
    let mut output = match &report.session {
        Some(session) => format!(
            "Session:  {} ({}, mirrord {})\nStarted:  {}\n",
            printable(&session.session_id),
            printable(&session.target),
            printable(&session.mirrord_version),
            printable(&session.started_at),
        ),
        None => format!("Session:  {}\n", printable(&report.session_id)),
    };
    render_session_start(&report.session_start, &mut output);
    render_application(report.application.as_ref(), now, &mut output);

    output
}

/// Whether `session` can be the id of an operator session: a `u64` in hex, which is also all
/// that ever goes into the request to the operator.
fn is_operator_session_id(session: &str) -> bool {
    (1..=16).contains(&session.len()) && session.chars().all(|char| char.is_ascii_hexdigit())
}

/// What decides which operator a request reaches: the kubeconfig, the context in it, and the
/// `baggage` header that picks one of several operators in a cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OperatorRoute {
    kubeconfig: Option<String>,
    kube_context: Option<String>,
    baggage: Option<String>,
}

impl OperatorRoute {
    /// The route to ask about a session: the one the session itself used, unless the user
    /// passed `--config-file` to this command.
    ///
    /// `resolved` is what this command's own configuration says. Without `--config-file` that
    /// is only defaults: the global configuration, the environment, the kubeconfig's current
    /// context. Any of them can have moved on since the session started, and asking another
    /// cluster would answer that the session is unknown. The session's kubeconfig and context
    /// are taken together, also when one of them is not set, because a context only means
    /// something in the kubeconfig it comes from.
    fn for_session(
        resolved: Self,
        session: Option<&SessionLabel>,
        config_file_given: bool,
    ) -> Self {
        match session {
            Some(session) if !config_file_given => Self {
                kubeconfig: session.kubeconfig.clone(),
                kube_context: session.kube_context.clone(),
                baggage: session.baggage.clone(),
            },
            _ => resolved,
        }
    }
}

/// Asks the operator about the session's start. Every way this can go wrong short of a broken
/// `--config-file` ends up in the report, because the application's numbers do not depend on it.
async fn ask_operator(
    session_id: &str,
    label: Option<&SessionLabel>,
    config_file: Option<&Path>,
) -> CliResult<SessionStart> {
    let mut context =
        ConfigContext::default().override_env_opt(LayerConfig::FILE_PATH_ENV, config_file);
    let mut config = match resolve_layer_config(&mut context).await {
        Ok(config) => config,
        Err(error) if config_file.is_some() => return Err(error),
        Err(error) => {
            return Ok(SessionStart::Unavailable {
                reason: error.to_string(),
            });
        }
    };
    let route = OperatorRoute::for_session(
        OperatorRoute {
            kubeconfig: config.kubeconfig.take(),
            kube_context: config.kube_context.take(),
            baggage: config.baggage.take(),
        },
        label,
        config_file.is_some(),
    );
    config.kubeconfig = route.kubeconfig;
    config.kube_context = route.kube_context;
    config.baggage = route.baggage;
    if !config.use_proxy {
        remove_proxy_env();
    }
    let namespace = label
        .and_then(|label| label.namespace.clone())
        .or_else(|| config.target.namespace.clone());

    let answer = tokio::time::timeout(OPERATOR_ANSWER_TIMEOUT, async {
        let mut analytics = NullReporter::default();
        let Some(api) = OperatorApi::try_new(&config, &mut analytics, &NullProgress).await? else {
            return Ok(None);
        };
        // A session without a namespace of its own ran in the kubeconfig's default one, which
        // is also where a user whose role covers only that namespace may read it.
        let namespace = match namespace {
            Some(namespace) => namespace,
            None => api.client().default_namespace().to_owned(),
        };
        api.session_start_timings(session_id, &namespace)
            .await
            .map(Some)
    })
    .await;

    Ok(match answer {
        Ok(Ok(Some(SessionStartTimings::Recorded {
            connects,
            missing_clusters,
        }))) => SessionStart::Recorded {
            connects,
            missing_clusters,
        },
        Ok(Ok(Some(SessionStartTimings::Forgotten))) => SessionStart::Forgotten,
        Ok(Ok(Some(SessionStartTimings::Unsupported))) => SessionStart::Unsupported,
        Ok(Ok(None)) => SessionStart::Unavailable {
            reason: "no mirrord operator found in the cluster".to_owned(),
        },
        Ok(Err(error)) => SessionStart::Unavailable {
            reason: error.to_string(),
        },
        Err(_) => SessionStart::Unavailable {
            reason: format!("no answer within {OPERATOR_ANSWER_TIMEOUT:?}"),
        },
    })
}

/// Why nothing can be shown for a session the user named, in words that follow "this machine has
/// no summary file for it".
fn unknown_session_reason(session_start: &SessionStart) -> Option<String> {
    match session_start {
        SessionStart::Recorded { .. } => None,
        SessionStart::NoOperator => Some("this machine has no summary file for it".to_owned()),
        SessionStart::Unsupported => Some(
            "this machine has no summary file for it, and the operator does not record session \
            start timings"
                .to_owned(),
        ),
        SessionStart::Forgotten => Some(
            "this machine has no summary file for it, and the operator has no start timings for \
            it"
            .to_owned(),
        ),
        SessionStart::Unavailable { reason } => Some(format!(
            "this machine has no summary file for it, and the operator could not be asked: \
            {reason}"
        )),
    }
}

/// Prints the timings of one session: the one named by `session`, or the latest one of this
/// machine.
///
/// The command fails only when it has nothing true to print: a summary file it cannot read, a
/// `--config-file` it cannot use, or a named session neither this machine nor the operator knows.
/// An operator that cannot be reached or has nothing to say is a line in the output.
pub(super) async fn diagnose_timings(
    session: Option<String>,
    config_file: Option<PathBuf>,
    json: bool,
) -> CliResult<()> {
    let file = match timings_dir() {
        Some(dir) => summary_file(&dir, session.as_deref())
            .map_err(|error| CliError::CallTimingsSummary { path: dir, error })?,
        None => None,
    };

    let (session_id, summary) = match (file, session) {
        (Some((path, updated)), _) => {
            let summary = fs::read(&path)
                .and_then(|content| {
                    serde_json::from_slice::<TimingsSummary>(&content).map_err(Into::into)
                })
                .map_err(|error| CliError::CallTimingsSummary { path, error })?;
            (summary.session.session_id.clone(), Some((summary, updated)))
        }
        (None, Some(session)) => (session, None),
        (None, None) if json => {
            // A script that asked for JSON gets JSON: the report's keys, all `null` because
            // there is no session to report on.
            println!(
                "{}",
                serde_json::json!({
                    "session_id": null,
                    "session": null,
                    "session_start": null,
                    "application": null,
                })
            );
            return Ok(());
        }
        (None, None) => {
            println!(
                "No timings yet. Run your application with `mirrord exec`: its session shows up \
                here as soon as the application starts.\n\
                Sessions with `experimental.call_timings` set to `false` and sessions started \
                with `mirrord container` leave no summary."
            );
            return Ok(());
        }
    };

    let label = summary.as_ref().map(|(summary, _)| &summary.session);
    let through_operator = label.is_none_or(|label| label.operator);
    let session_start = if through_operator && is_operator_session_id(&session_id) {
        ask_operator(&session_id, label, config_file.as_deref()).await?
    } else {
        SessionStart::NoOperator
    };

    if summary.is_none()
        && let Some(reason) = unknown_session_reason(&session_start)
    {
        return Err(CliError::CallTimingsSessionUnknown {
            session: session_id,
            reason,
        });
    }

    let report = Report {
        session_id,
        session_start,
        // The routing header and the kubeconfig are only there to find the session's operator
        // again. The header can carry tracing ids of whatever started the session and the
        // kubeconfig names the user's home directory, which have no place in a report people
        // paste into tickets.
        session: label.cloned().map(|label| SessionLabel {
            baggage: None,
            kubeconfig: None,
            ..label
        }),
        application: summary.map(|(summary, updated)| Application {
            updated_at: humantime::format_rfc3339_seconds(updated).to_string(),
            updated,
            suggestions: suggestions(&summary),
            kinds: summary.kinds,
            slowest_calls: summary.slowest_calls,
            path_lookups: summary.path_lookups,
            untracked_path_lookups: summary.untracked_path_lookups,
        }),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render(&report, SystemTime::now()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(path_lookups: Vec<DirLookups>) -> TimingsSummary {
        TimingsSummary {
            session: SessionLabel {
                session_id: "5f0c".to_owned(),
                target: "deployment/orders".to_owned(),
                mirrord_version: "3.291.1".to_owned(),
                started_at: "2026-11-25T10:00:00Z".to_owned(),
                operator: true,
                kube_context: None,
                kubeconfig: None,
                namespace: None,
                baggage: None,
            },
            kinds: vec![
                KindSummary {
                    kind: "name_lookup".to_owned(),
                    calls: 2,
                    total_nanos: 1_700_000_000,
                    slowest_nanos: 1_200_000_000,
                },
                KindSummary {
                    kind: "file_stat".to_owned(),
                    calls: 175,
                    total_nanos: 1_300_000_000,
                    slowest_nanos: 45_300_000,
                },
            ],
            slowest_calls: vec![
                SlowCallSummary {
                    kind: "name_lookup".to_owned(),
                    subject: Some("orders-db.internal".to_owned()),
                    nanos: 1_200_000_000,
                },
                SlowCallSummary {
                    kind: "file_read".to_owned(),
                    subject: None,
                    nanos: 150_000_000,
                },
                SlowCallSummary {
                    kind: "port_subscribe".to_owned(),
                    subject: None,
                    nanos: 120_000_000,
                },
            ],
            path_lookups,
            untracked_path_lookups: 0,
        }
    }

    fn report(summary: TimingsSummary, session_start: SessionStart) -> Report {
        let updated = SystemTime::UNIX_EPOCH + Duration::from_secs(1_795_600_000);
        Report {
            session_id: summary.session.session_id.clone(),
            session: Some(summary.session.clone()),
            session_start,
            application: Some(Application {
                updated_at: humantime::format_rfc3339_seconds(updated).to_string(),
                updated,
                suggestions: suggestions(&summary),
                kinds: summary.kinds,
                slowest_calls: summary.slowest_calls,
                path_lookups: summary.path_lookups,
                untracked_path_lookups: summary.untracked_path_lookups,
            }),
        }
    }

    fn phase(name: &str, duration_ms: u64) -> StartPhase {
        StartPhase {
            name: name.to_owned(),
            description: String::new(),
            started_after_ms: None,
            duration_ms,
            failed: false,
        }
    }

    /// The rows of every table in `rendered`, cell by cell.
    fn table_rows(rendered: &str) -> Vec<Vec<&str>> {
        rendered
            .lines()
            .filter(|line| line.starts_with('|'))
            .map(|line| {
                line.split('|')
                    .skip(1)
                    .map(str::trim)
                    .collect::<Vec<_>>()
                    .split_last()
                    .map(|(_, cells)| cells.to_vec())
                    .unwrap_or_default()
            })
            .collect()
    }

    fn dir(dir: &str, lookups: u64, not_found: u64, total_nanos: u64) -> DirLookups {
        DirLookups {
            dir: dir.to_owned(),
            lookups,
            not_found,
            total_nanos,
        }
    }

    fn suggested(summary: &TimingsSummary) -> Vec<(String, u64)> {
        not_found_dirs(summary)
            .into_iter()
            .map(|tree| (tree.dir, tree.lookups))
            .collect()
    }

    #[test]
    fn waits_are_printed_in_the_unit_that_fits() {
        assert_eq!(format_wait(0), "0 ns");
        assert_eq!(format_wait(999), "999 ns");
        assert_eq!(format_wait(85_400), "85 µs");
        assert_eq!(format_wait(12_340_000), "12.3 ms");
        assert_eq!(format_wait(1_700_000_000), "1.70 s");
    }

    #[test]
    fn the_application_section_lists_every_kind_a_row_for_all_and_the_slowest_calls() {
        let report = report(summary(vec![]), SessionStart::NoOperator);
        let now = report.application.as_ref().unwrap().updated + Duration::from_secs(65);

        let rendered = render(&report, now);

        assert!(rendered.contains("Session:  5f0c (deployment/orders, mirrord 3.291.1)"));
        assert!(rendered.contains("\nApplication calls\n"), "{rendered}");
        assert!(rendered.contains("(1m 5s ago)"), "{rendered}");
        assert_eq!(
            table_rows(&rendered),
            [
                vec!["Kind", "Calls", "Total", "Slowest"],
                vec!["name lookup", "2", "1.70 s", "1.20 s"],
                vec!["file stat", "175", "1.30 s", "45.3 ms"],
                vec!["all", "177", "3.00 s", "1.20 s"],
            ]
        );
        let slowest = rendered
            .lines()
            .skip_while(|line| *line != "Slowest calls:")
            .skip(1)
            .take(3)
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>();
        assert_eq!(
            slowest,
            [
                "1.20 s name lookup orders-db.internal",
                "150.0 ms file read (an open file)",
                "120.0 ms port subscribe",
            ],
            "only a file call without a path is about an open file"
        );
        assert!(!rendered.contains("Suggestions"));
        assert!(
            !rendered.contains("Session start"),
            "a session without an operator has no start section"
        );
    }

    #[test]
    fn a_session_without_remote_calls_says_so_instead_of_an_empty_table() {
        let mut summary = summary(vec![]);
        summary.kinds.clear();
        summary.slowest_calls.clear();
        let report = report(summary, SessionStart::NoOperator);

        let rendered = render(&report, SystemTime::now());

        assert!(rendered.contains("made no calls"));
        assert!(!rendered.contains('|'));
    }

    /// A path, a target name and the operator's words are chosen outside this command. One that
    /// carries a terminal control sequence must reach the terminal as text.
    #[test]
    fn control_characters_from_outside_are_printed_as_escapes() {
        let mut summary = summary(vec![dir("/app/\u{1b}[2Jvendor", 150, 150, 900_000_000)]);
        summary.session.target = "deployment/\u{1b}[2Jorders".to_owned();
        summary.slowest_calls = vec![SlowCallSummary {
            kind: "file_open".to_owned(),
            subject: Some("/app/\u{1b}[2Jconfig".to_owned()),
            nanos: 1_000_000,
        }];
        let connects = vec![StartConnect {
            cluster: None,
            started_at: "2026-11-25T01:11:54.660513Z".to_owned(),
            phases: vec![StartPhase {
                description: "finding the \u{1b}[2Jtarget".to_owned(),
                ..phase("target_lookup", 1)
            }],
        }];
        let recorded = report(
            summary.clone(),
            SessionStart::Recorded {
                connects,
                missing_clusters: Vec::new(),
            },
        );
        let unavailable = report(
            summary,
            SessionStart::Unavailable {
                reason: "no route to \u{1b}[2Jhost".to_owned(),
            },
        );

        for report in [recorded, unavailable] {
            let rendered = render(&report, SystemTime::now());
            assert!(!rendered.contains('\u{1b}'), "{rendered:?}");
            assert!(rendered.contains("\\u{1b}[2J"), "{rendered:?}");
        }
    }

    /// Tests in the operator's repository match on the heading and on the operator's own phase
    /// names, so both are printed as they are. The operator describes its phases itself, so a
    /// phase this version has never heard of comes with its description, and a phase from an
    /// operator that sends none still gets its row.
    #[test]
    fn the_session_start_section_prints_the_operators_phases_as_they_come() {
        let connects = vec![
            StartConnect {
                cluster: None,
                started_at: "2026-11-25T01:11:54.660513Z".to_owned(),
                phases: vec![
                    StartPhase {
                        description: "finding the target".to_owned(),
                        ..phase("target_lookup", 1_293)
                    },
                    StartPhase {
                        description: "spinning up the warp drive".to_owned(),
                        ..phase("warp_drive_spinup", 7)
                    },
                    StartPhase {
                        failed: true,
                        ..phase("queue_splits", 72_824)
                    },
                ],
            },
            StartConnect {
                cluster: None,
                started_at: "2026-11-25T01:13:15.161211Z".to_owned(),
                phases: vec![phase("agent_reused", 0)],
            },
            StartConnect {
                cluster: None,
                started_at: "2026-11-25T01:13:16.000000Z".to_owned(),
                phases: vec![],
            },
        ];
        let report = report(
            summary(vec![]),
            SessionStart::Recorded {
                connects,
                missing_clusters: Vec::new(),
            },
        );

        let rendered = render(&report, SystemTime::now());

        let start = rendered.find("\nSession start\n").expect("the heading");
        let application = rendered.find("\nApplication calls\n").expect("the heading");
        assert!(
            start < application,
            "the session starts before its application runs"
        );
        let rows = table_rows(&rendered);
        assert_eq!(
            rows.get(..6),
            Some(
                [
                    vec!["Connect", "Phase", "What it is", "Duration"],
                    vec!["1", "target_lookup", "finding the target", "1.29 s"],
                    vec![
                        "1",
                        "warp_drive_spinup",
                        "spinning up the warp drive",
                        "7.0 ms"
                    ],
                    vec!["1", "queue_splits", "", "72.82 s (did not finish)"],
                    vec!["2", "agent_reused", "", "0 ms"],
                    vec!["3", "", "no phase finished yet", ""],
                ]
                .as_slice()
            )
        );
        assert_eq!(rows.get(6), Some(&vec!["all", "", "", "74.12 s"]));
    }

    /// The three answers that are not timings each get one plain line under the heading, and
    /// the application's numbers are printed all the same.
    #[test]
    fn an_operator_with_nothing_to_say_is_one_line_not_a_failure() {
        for (session_start, line) in [
            (
                SessionStart::Unsupported,
                "This operator does not record session start timings.",
            ),
            (
                SessionStart::Forgotten,
                "The operator has no start timings for this session.",
            ),
            (
                SessionStart::Unavailable {
                    reason: "no answer within 20s".to_owned(),
                },
                "Could not ask the operator: no answer within 20s",
            ),
        ] {
            let rendered = render(&report(summary(vec![]), session_start), SystemTime::now());

            assert!(
                rendered.contains(&format!("\nSession start\n{line}")),
                "{rendered}"
            );
            assert!(rendered.contains("| name lookup |"), "{rendered}");
        }
    }

    /// Scripts read the JSON by section, so it is one object with a key per section and the
    /// operator's part says in `status` which of its cases this is.
    #[test]
    fn the_json_report_has_a_key_per_section() {
        let connects = vec![StartConnect {
            cluster: None,
            started_at: "2026-11-25T01:11:54.660513Z".to_owned(),
            phases: vec![phase("target_lookup", 1_293)],
        }];
        let report = report(
            summary(vec![]),
            SessionStart::Recorded {
                connects,
                missing_clusters: Vec::new(),
            },
        );

        let json = serde_json::to_value(&report).unwrap();
        let at = |pointer: &str| json.pointer(pointer).cloned().unwrap_or_default();

        assert_eq!(at("/session_id"), "5f0c");
        assert_eq!(at("/session/target"), "deployment/orders");
        assert_eq!(at("/session_start/status"), "recorded");
        assert_eq!(
            at("/session_start/connects/0/phases/0"),
            serde_json::json!({ "name": "target_lookup", "durationMs": 1293 })
        );
        assert_eq!(at("/application/kinds/0/kind"), "name_lookup");
        assert_eq!(
            at("/application/slowest_calls/0/subject"),
            "orders-db.internal"
        );
        assert_eq!(at("/application/updated_at"), "2026-11-25T09:46:40Z");

        let no_operator = serde_json::to_value(SessionStart::NoOperator).unwrap();
        assert_eq!(no_operator, serde_json::json!({ "status": "no_operator" }));
    }

    /// A session started against one cluster must be asked about in that cluster. The command
    /// used to keep its own resolved context whenever there was one, and there always is one
    /// once the user has a global default: change that default after the session started, and
    /// the command asked the new cluster, which answered that it never saw the session.
    #[test]
    fn the_sessions_own_route_is_asked_unless_a_config_file_is_passed() {
        let mut session = summary(vec![]).session;
        session.kubeconfig = Some("/home/dev/.kube/work".to_owned());
        session.kube_context = Some("cluster-a".to_owned());
        session.baggage = None;
        let todays_defaults = OperatorRoute {
            kubeconfig: None,
            kube_context: Some("cluster-b".to_owned()),
            baggage: Some("mirrord-session=someone-else".to_owned()),
        };

        assert_eq!(
            OperatorRoute::for_session(todays_defaults.clone(), Some(&session), false),
            OperatorRoute {
                kubeconfig: Some("/home/dev/.kube/work".to_owned()),
                kube_context: Some("cluster-a".to_owned()),
                baggage: None,
            },
            "all three come from the session, also the one it did not set"
        );
        assert_eq!(
            OperatorRoute::for_session(todays_defaults.clone(), Some(&session), true),
            todays_defaults,
            "`--config-file` is the user pointing this command somewhere else"
        );
        assert_eq!(
            OperatorRoute::for_session(todays_defaults.clone(), None, false),
            todays_defaults,
            "a session this machine has no file for has no route of its own"
        );
    }

    #[test]
    fn only_a_hex_number_is_sent_to_the_operator_as_a_session_id() {
        assert!(is_operator_session_id("72972C26C9D0A943"));
        assert!(is_operator_session_id("5e1d5b32275a0332"));
        assert!(!is_operator_session_id(""));
        assert!(
            !is_operator_session_id("72972C26C9D0A9431"),
            "longer than a u64"
        );
        assert!(!is_operator_session_id(
            "3f2b6c1e-0d9a-4c57-9a55-0d8f6f1e2a11"
        ));
        assert!(!is_operator_session_id("1&namespace=kube-system"));
    }

    /// The only directory worth pointing at is one where the cluster never had anything: a
    /// single found file means the application needs the cluster for that directory.
    #[test]
    fn a_directory_is_suggested_only_when_every_lookup_in_it_was_not_found() {
        let summary = summary(vec![
            dir("/app/vendor-1.2", 150, 150, 900_000_000),
            dir("/app/config", 40, 39, 300_000_000),
            dir("/srv/few", 19, 19, 100_000_000),
        ]);

        assert_eq!(suggested(&summary), [("/app/vendor-1.2".to_owned(), 150)]);

        let suggestions = suggestions(&summary);
        let [suggestion] = suggestions.as_slice() else {
            panic!("expected one suggestion, got {suggestions:?}");
        };
        assert!(suggestion.contains("150 file lookups under `/app/vendor-1.2`"));
        assert!(
            suggestion.contains(r#"Add "(?-i)^/app/vendor-1\\.2/" to `feature.fs.not_found`"#),
            "the pattern is a JSON string that matches the directory's name literally, in the \
            case it was looked up in: {suggestion}"
        );
        assert!(suggestion.contains("about 900.0 ms"));

        // File patterns are matched ignoring case, as `feature.fs` builds them.
        let pattern = suggestion
            .split_once("Add ")
            .and_then(|(_, rest)| rest.split_once(" to `feature.fs.not_found`"))
            .map(|(pattern, _)| serde_json::from_str::<String>(pattern).unwrap())
            .expect("the suggestion names a pattern");
        let filter = regex::RegexSetBuilder::new([pattern])
            .case_insensitive(true)
            .build()
            .unwrap();
        assert!(filter.is_match("/app/vendor-1.2/lib.so"));
        assert!(
            !filter.is_match("/App/vendor-1.2/lib.so"),
            "the session never looked the directory up in another case"
        );
    }

    /// A pattern for a directory also covers everything below it, so a file found deeper down
    /// rules the directory out even when its own lookups all failed.
    #[test]
    fn a_found_file_below_a_directory_rules_the_directory_out() {
        let summary = summary(vec![
            dir("/opt/app", 30, 30, 500_000_000),
            dir("/opt/app/plugins", 25, 24, 400_000_000),
            dir("/opt/application", 30, 30, 100_000_000),
        ]);

        assert_eq!(
            suggested(&summary),
            [("/opt/application".to_owned(), 30)],
            "`/opt/application` is not below `/opt/app`, so the found file does not touch it"
        );
    }

    #[test]
    fn one_tree_gets_one_suggestion_with_the_numbers_of_everything_below_it() {
        let summary = summary(vec![
            dir("/data/cache", 10, 10, 100_000_000),
            dir("/data/cache/a", 30, 30, 200_000_000),
            dir("/data/cache/b", 25, 25, 300_000_000),
        ]);

        assert_eq!(suggested(&summary), [("/data/cache".to_owned(), 65)]);
    }

    #[test]
    fn at_most_three_suggestions_the_slowest_first_and_never_the_root() {
        let summary = summary(vec![
            dir("/", 500, 500, 9_000_000_000),
            dir("/a", 20, 20, 100),
            dir("/b", 20, 20, 400),
            dir("/c", 20, 20, 300),
            dir("/d", 20, 20, 200),
        ]);

        let dirs = suggested(&summary)
            .into_iter()
            .map(|(dir, _)| dir)
            .collect::<Vec<_>>();
        assert_eq!(dirs, ["/b", "/c", "/d"]);
    }

    /// Lookups past the proxy's directory cap are missing from the summary, and any of them may
    /// have found a file below a directory that looks empty. Nothing is certain then.
    #[test]
    fn no_suggestion_when_the_summary_lost_track_of_lookups() {
        let mut summary = summary(vec![dir("/app/vendor", 150, 150, 900_000_000)]);
        summary.untracked_path_lookups = 1;

        assert!(suggestions(&summary).is_empty());
    }

    #[test]
    fn a_summary_is_found_by_session_name_or_as_the_one_modified_last() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            summary_file(&dir.path().join("missing"), None).unwrap(),
            None
        );
        assert_eq!(summary_file(dir.path(), None).unwrap(), None);

        let at = |seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
        for (name, modified) in [
            ("AB12.json", 100),
            ("new.json", 300),
            ("newer.partial", 400),
        ] {
            let path = dir.path().join(name);
            fs::write(&path, "{}").unwrap();
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(at(modified))
                .unwrap();
        }

        assert_eq!(
            summary_file(dir.path(), None).unwrap(),
            Some((dir.path().join("new.json"), at(300)))
        );
        assert_eq!(
            summary_file(dir.path(), Some("ab12")).unwrap(),
            Some((dir.path().join("AB12.json"), at(100))),
            "a hex id is found in either case"
        );
        assert_eq!(summary_file(dir.path(), Some("other")).unwrap(), None);
        assert_eq!(
            summary_file(dir.path(), Some("../new")).unwrap(),
            None,
            "a name is never used as a path"
        );
    }
}
