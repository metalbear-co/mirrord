//! The session's summary of what the application waited on mirrord for, kept in a small file
//! that `mirrord diagnose timings` reads.
//!
//! The layers measure their own remote calls and send the totals as
//! [`CallTimingsSnapshot`]s. A process can be killed at any moment and `mirrord exec` leaves no
//! CLI process behind, so the internal proxy, which outlives every layer of its session, is the
//! one that keeps the numbers: [`SessionTimings`] adds the layers up and [`CallTimingsRecorder`]
//! writes the result to `<timings directory>/<session id>.json`.
//!
//! The proxy also sees every file request with its path, which the layers' counters leave out to
//! stay cheap. It tallies path lookups per directory and how many of them the cluster answered
//! with "not found", which is what the command's suggestions are built on.

use std::{
    collections::{BTreeMap, HashMap},
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use mirrord_intproxy_protocol::{
    CallKind, CallTimingsSnapshot, KindTimings, LayerId, MAX_SLOW_CALLS, MessageId,
    ProxyToLayerMessage, SlowCall,
};
use mirrord_protocol::{
    ErrorKindInternal, FileRequest, FileResponse, RemoteIOError, ResponseError,
};
use serde::{Deserialize, Serialize};
use tokio::{sync::watch, task::JoinHandle};

/// How many sessions' summaries stay in the timings directory for good.
///
/// The command prints the latest session unless one is named, so older files are only there for
/// a user who wants to compare with a previous run. Ten covers that and keeps the directory from
/// growing with every session ever started.
pub const KEPT_SESSIONS: usize = 10;

/// How old a summary has to be before a newer session may remove it.
///
/// Counting files alone would let the eleventh of many sessions running side by side (a test
/// suite, a dozen services started together) delete the file of one that is still running. A
/// file touched within the last day is therefore never removed, whatever the count. A summary is
/// a couple of kilobytes, so even a thousand sessions in a day stay small.
const PRUNE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Extension of a session's summary file.
pub const SUMMARY_EXTENSION: &str = "json";

/// How many directories [`SessionTimings`] tallies path lookups for.
///
/// Each one is a map entry with a path of at most [`DIR_KEY_DEPTH`] components, so this bounds
/// the proxy's memory and the size of the summary file for applications that walk huge trees.
/// Lookups past the cap are only counted, in [`TimingsSummary::untracked_path_lookups`].
const MAX_TRACKED_DIRS: usize = 128;

/// How many leading path components identify a directory in the tally.
///
/// Deeper directories are counted under their ancestor at this depth. Four is deep enough to
/// tell `/app/vendor/bundle/ruby` from `/app/config`, and shallow enough that a dependency tree
/// with thousands of directories folds into a handful of entries.
const DIR_KEY_DEPTH: usize = 4;

/// The shortest time between two writes of the summary file.
///
/// Every layer sends a snapshot every couple of seconds, and a session can have many layers.
/// One write per second keeps the file fresh enough for a person to read while the session runs.
const WRITE_INTERVAL: Duration = Duration::from_secs(1);

/// How long the proxy waits for the last write of the summary file when it exits.
///
/// The file lives in the user's home directory, which can be a slow network mount. The proxy
/// must not hang on it, so past this the last summary is dropped and the file keeps the previous
/// one.
const FINAL_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// What identifies a session to the person reading its summary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLabel {
    pub session_id: String,
    pub target: String,
    pub mirrord_version: String,
    /// RFC 3339 time the internal proxy started.
    pub started_at: String,
    /// Whether the session runs through the operator, which then has the session's start
    /// timings under the same `session_id`.
    #[serde(default)]
    pub operator: bool,
    /// The kube context the session was started with, when the configuration names one. The
    /// command uses it to ask the same cluster about the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kube_context: Option<String>,
    /// The target's namespace, when the configuration names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    /// The `baggage` header the session's requests carry, which picks the operator that serves
    /// them where a cluster runs more than one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baggage: Option<String>,
}

/// The totals of one kind of call across every process of the session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindSummary {
    pub kind: String,
    pub calls: u64,
    pub total_nanos: u64,
    pub slowest_nanos: u64,
}

/// One of the slowest single calls of the session, with what it was for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlowCallSummary {
    pub kind: String,
    /// The host of a name lookup, the path of a file operation, the address of an outgoing
    /// connection. [`None`] for a call on an already open file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub nanos: u64,
}

/// The path lookups (open, stat, access, readlink) that went to the cluster for paths under one
/// directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirLookups {
    pub dir: String,
    pub lookups: u64,
    /// How many of them the cluster answered with "not found".
    pub not_found: u64,
    /// Time from the proxy receiving the requests to it sending the answers. A little less than
    /// what the application waited, which also includes the hop between the layer and the proxy.
    pub total_nanos: u64,
}

/// The content of a session's summary file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimingsSummary {
    pub session: SessionLabel,
    /// Slowest kind first.
    pub kinds: Vec<KindSummary>,
    /// The slowest single calls of the session, slowest first. Only calls that took long enough
    /// for the layer to remember them are here, so a fast session has none.
    #[serde(default)]
    pub slowest_calls: Vec<SlowCallSummary>,
    /// Slowest directory first.
    pub path_lookups: Vec<DirLookups>,
    /// Lookups that are missing from `path_lookups` because [`MAX_TRACKED_DIRS`] directories
    /// were already tracked. When this is not zero, `path_lookups` is not the whole picture.
    #[serde(default)]
    pub untracked_path_lookups: u64,
}

#[derive(Clone, Copy, Default)]
struct KindTotals {
    calls: u64,
    total_nanos: u64,
    slowest_nanos: u64,
}

impl KindTotals {
    fn add(&mut self, timings: &KindTimings) {
        self.calls = self.calls.saturating_add(timings.calls);
        self.total_nanos = self.total_nanos.saturating_add(timings.total_nanos);
        self.slowest_nanos = self.slowest_nanos.max(timings.slowest_nanos);
    }
}

#[derive(Default)]
struct DirTally {
    lookups: u64,
    not_found: u64,
    total_nanos: u64,
}

struct PendingLookup {
    dir: String,
    received: Instant,
}

/// The session's numbers, as a plain state machine with no IO.
pub struct SessionTimings {
    session: SessionLabel,
    /// The latest snapshot of every connected layer. A snapshot holds totals since its process
    /// started, so a new one replaces the previous one.
    live_layers: HashMap<LayerId, CallTimingsSnapshot>,
    /// The last snapshots of the layers that are gone, added up. A session can run thousands of
    /// short processes, so their snapshots are not kept one by one.
    closed_layers: HashMap<CallKind, KindTotals>,
    /// The slowest calls of the layers that are gone, at most [`MAX_SLOW_CALLS`].
    closed_slowest: Vec<SlowCall>,
    /// Path lookups the cluster has not answered yet.
    pending_lookups: HashMap<(LayerId, MessageId), PendingLookup>,
    dirs: BTreeMap<String, DirTally>,
    untracked_path_lookups: u64,
}

impl SessionTimings {
    pub fn new(session: SessionLabel) -> Self {
        Self {
            session,
            live_layers: HashMap::new(),
            closed_layers: HashMap::new(),
            closed_slowest: Vec::new(),
            pending_lookups: HashMap::new(),
            dirs: BTreeMap::new(),
            untracked_path_lookups: 0,
        }
    }

    pub fn layer_snapshot(&mut self, layer_id: LayerId, snapshot: CallTimingsSnapshot) {
        self.live_layers.insert(layer_id, snapshot);
    }

    /// Keeps the numbers of a layer whose process is gone, and forgets its unanswered lookups.
    pub fn layer_closed(&mut self, layer_id: LayerId) {
        if let Some(snapshot) = self.live_layers.remove(&layer_id) {
            for timings in &snapshot.kinds {
                self.closed_layers
                    .entry(timings.kind)
                    .or_default()
                    .add(timings);
            }
            self.closed_slowest.extend(snapshot.slowest);
            keep_slowest(&mut self.closed_slowest);
        }
        self.pending_lookups
            .retain(|(pending_layer, _), _| *pending_layer != layer_id);
    }

    /// Remembers a path lookup until [`Self::layer_response`] sees its answer.
    pub fn file_request(
        &mut self,
        layer_id: LayerId,
        message_id: MessageId,
        request: &FileRequest,
        now: Instant,
    ) {
        let Some(dir) = looked_up_path(request).and_then(dir_key) else {
            return;
        };

        self.pending_lookups
            .insert((layer_id, message_id), PendingLookup { dir, received: now });
    }

    /// Counts the answer to a path lookup under its directory.
    pub fn layer_response(
        &mut self,
        layer_id: LayerId,
        message_id: MessageId,
        response: &ProxyToLayerMessage,
        now: Instant,
    ) {
        let Some(lookup) = self.pending_lookups.remove(&(layer_id, message_id)) else {
            return;
        };

        if !self.dirs.contains_key(&lookup.dir) && self.dirs.len() >= MAX_TRACKED_DIRS {
            if self.untracked_path_lookups == 0 {
                tracing::debug!(
                    tracked = MAX_TRACKED_DIRS,
                    dir = lookup.dir,
                    "Path lookups in more directories than the timings summary tracks, \
                    the rest are only counted."
                );
            }
            self.untracked_path_lookups = self.untracked_path_lookups.saturating_add(1);
            return;
        }

        let waited = now.saturating_duration_since(lookup.received);
        let tally = self.dirs.entry(lookup.dir).or_default();
        tally.lookups = tally.lookups.saturating_add(1);
        tally.not_found = tally
            .not_found
            .saturating_add(u64::from(is_not_found(response)));
        tally.total_nanos = tally
            .total_nanos
            .saturating_add(u64::try_from(waited.as_nanos()).unwrap_or(u64::MAX));
    }

    pub fn summary(&self) -> TimingsSummary {
        let mut totals = self.closed_layers.clone();
        for timings in self
            .live_layers
            .values()
            .flat_map(|snapshot| &snapshot.kinds)
        {
            totals.entry(timings.kind).or_default().add(timings);
        }

        let mut kinds = totals
            .into_iter()
            .map(|(kind, totals)| KindSummary {
                kind: <&str>::from(kind).to_owned(),
                calls: totals.calls,
                total_nanos: totals.total_nanos,
                slowest_nanos: totals.slowest_nanos,
            })
            .collect::<Vec<_>>();
        // The name breaks ties so the same numbers always print in the same order.
        kinds.sort_by(|a, b| {
            b.total_nanos
                .cmp(&a.total_nanos)
                .then_with(|| a.kind.cmp(&b.kind))
        });

        let mut slowest = self
            .live_layers
            .values()
            .flat_map(|snapshot| &snapshot.slowest)
            .chain(&self.closed_slowest)
            .cloned()
            .collect::<Vec<_>>();
        keep_slowest(&mut slowest);
        let slowest_calls = slowest
            .into_iter()
            .map(|call| SlowCallSummary {
                kind: <&str>::from(call.kind).to_owned(),
                subject: call.subject,
                nanos: call.nanos,
            })
            .collect();

        let mut path_lookups = self
            .dirs
            .iter()
            .map(|(dir, tally)| DirLookups {
                dir: dir.clone(),
                lookups: tally.lookups,
                not_found: tally.not_found,
                total_nanos: tally.total_nanos,
            })
            .collect::<Vec<_>>();
        path_lookups.sort_by(|a, b| {
            b.total_nanos
                .cmp(&a.total_nanos)
                .then_with(|| a.dir.cmp(&b.dir))
        });

        TimingsSummary {
            session: self.session.clone(),
            kinds,
            slowest_calls,
            path_lookups,
            untracked_path_lookups: self.untracked_path_lookups,
        }
    }
}

/// Sorts `calls` slowest first and drops all but the [`MAX_SLOW_CALLS`] slowest.
fn keep_slowest(calls: &mut Vec<SlowCall>) {
    // The subject breaks ties so the same calls always list in the same order.
    calls.sort_by(|a, b| {
        b.nanos
            .cmp(&a.nanos)
            .then_with(|| a.subject.cmp(&b.subject))
    });
    calls.truncate(MAX_SLOW_CALLS);
}

/// The path a request looks up by name, for the requests that only look: opening, checking and
/// reading metadata. Requests on an already open file have no path, and requests that change the
/// file system are left out because "not found" is not their usual answer.
fn looked_up_path(request: &FileRequest) -> Option<&Path> {
    match request {
        FileRequest::Open(request) => Some(&request.path),
        FileRequest::Access(request) => Some(&request.pathname),
        FileRequest::Xstat(request) => request.path.as_deref(),
        FileRequest::ReadLink(request) => Some(&request.path),
        FileRequest::StatFs(request) => Some(&request.path),
        FileRequest::StatFsV2(request) => Some(&request.path),
        _ => None,
    }
}

/// The directory a path is counted under: its parent, cut to [`DIR_KEY_DEPTH`] components.
///
/// # Returns
///
/// [`None`] for a path that is not absolute in the cluster's (unix) form or is not valid UTF-8.
fn dir_key(path: &Path) -> Option<String> {
    let path = path.to_str()?.strip_prefix('/')?;
    let mut components = path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    // The last component is the file or directory being looked up.
    components.pop();
    components.truncate(DIR_KEY_DEPTH);

    Some(format!("/{}", components.join("/")))
}

fn is_not_found(response: &ProxyToLayerMessage) -> bool {
    let ProxyToLayerMessage::File(response) = response else {
        return false;
    };

    let error = match response {
        FileResponse::Open(result) => result.as_ref().err(),
        FileResponse::Access(result) => result.as_ref().err(),
        FileResponse::Xstat(result) => result.as_ref().err(),
        FileResponse::XstatFs(result) => result.as_ref().err(),
        FileResponse::XstatFsV2(result) => result.as_ref().err(),
        FileResponse::ReadLink(result) => result.as_ref().err(),
        _ => None,
    };

    matches!(
        error,
        Some(ResponseError::RemoteIO(RemoteIOError {
            kind: ErrorKindInternal::NotFound,
            ..
        }))
    )
}

/// Keeps a session's [`SessionTimings`] and writes its summary file.
///
/// Writing happens on a task of its own, off the proxy's main loop, because the file is in the
/// user's home directory and a slow disk there must not hold up the application's traffic.
pub struct CallTimingsRecorder {
    timings: SessionTimings,
    latest: watch::Sender<Option<TimingsSummary>>,
    /// Ends the writer's pause between two writes. Dropping it does the same.
    write_now: watch::Sender<()>,
    writer: JoinHandle<()>,
}

impl CallTimingsRecorder {
    /// Starts the task that writes the session's summary into `dir`. Must be called from a tokio
    /// runtime.
    pub fn start(dir: PathBuf, session: SessionLabel) -> Self {
        let (latest, summaries) = watch::channel(None);
        let (write_now, pause_over) = watch::channel(());
        let path = dir.join(format!("{}.{SUMMARY_EXTENSION}", session.session_id));
        let writer = tokio::spawn(write_summaries(path, summaries, pause_over));

        Self {
            timings: SessionTimings::new(session),
            latest,
            write_now,
            writer,
        }
    }

    pub fn timings(&mut self) -> &mut SessionTimings {
        &mut self.timings
    }

    /// Hands the current summary to the writer task, which writes it once [`WRITE_INTERVAL`] has
    /// passed since its last write. Only the newest summary is kept when the writer is behind.
    pub fn publish(&self) {
        self.latest.send_replace(Some(self.timings.summary()));
    }

    /// Like [`Self::publish`], but the summary is written right away.
    ///
    /// For when a process of the session has just exited: that is the moment a person runs
    /// `mirrord diagnose timings`, and the file must already hold the process's last numbers.
    pub fn publish_now(&self) {
        self.publish();
        self.write_now.send_replace(());
    }

    /// Waits for the last summary to reach the file, for at most [`FINAL_WRITE_TIMEOUT`].
    pub async fn finish(self) {
        let Self {
            latest,
            write_now,
            writer,
            ..
        } = self;
        drop(latest);
        drop(write_now);

        if tokio::time::timeout(FINAL_WRITE_TIMEOUT, writer)
            .await
            .is_err()
        {
            tracing::warn!(
                timeout = ?FINAL_WRITE_TIMEOUT,
                "The last call timings summary was not written in time, \
                the file keeps the previous one."
            );
        }
    }
}

/// Writes every new summary to `path` until the recorder is dropped, pausing for
/// [`WRITE_INTERVAL`] after each write unless `pause_over` says to write now.
///
/// Stops at the first failed write: a directory that cannot be written now will not be writable
/// a second later, and the summary is not worth a warning every second.
async fn write_summaries(
    path: PathBuf,
    mut summaries: watch::Receiver<Option<TimingsSummary>>,
    mut pause_over: watch::Receiver<()>,
) {
    let mut first_write = true;

    while summaries.changed().await.is_ok() {
        let Some(summary) = summaries.borrow_and_update().clone() else {
            continue;
        };

        let write = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || {
                if first_write {
                    prepare_dir(&path)?;
                }
                write_summary(&path, &summary)
            })
        };
        match write.await {
            Ok(Ok(())) => first_write = false,
            Ok(Err(error)) => {
                tracing::warn!(
                    %error,
                    path = %path.display(),
                    "Failed to write the call timings summary, \
                    `mirrord diagnose timings` will not show this session."
                );
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "The task writing the call timings summary failed.");
                return;
            }
        }

        // A dropped recorder also ends the pause, so the last summary is written at once and
        // the loop then ends on the closed `summaries` channel.
        tokio::select! {
            _ = tokio::time::sleep(WRITE_INTERVAL) => {}
            _ = pause_over.changed() => {}
        }
    }
}

/// Creates the timings directory and removes the summaries that are both beyond the
/// [`KEPT_SESSIONS`] newest (counting this session's file) and older than [`PRUNE_AFTER`].
fn prepare_dir(summary_path: &Path) -> io::Result<()> {
    let Some(dir) = summary_path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(dir)?;

    let summaries = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path != summary_path
                && path
                    .extension()
                    .is_some_and(|extension| extension == SUMMARY_EXTENSION)
        })
        .filter_map(|path| {
            let modified = fs::metadata(&path).and_then(|meta| meta.modified()).ok()?;
            Some((path, modified))
        })
        .collect();

    let stale = stale_summaries(
        summaries,
        KEPT_SESSIONS.saturating_sub(1),
        SystemTime::now(),
    );
    for stale in stale {
        // Another session's proxy may be removing the same file right now.
        if let Err(error) = fs::remove_file(&stale)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::debug!(%error, path = %stale.display(), "Failed to remove an old call timings summary.");
        }
    }

    Ok(())
}

/// The files that are neither among the `keep` most recently modified ones nor modified within
/// [`PRUNE_AFTER`] of `now`.
fn stale_summaries(
    mut files: Vec<(PathBuf, SystemTime)>,
    keep: usize,
    now: SystemTime,
) -> Vec<PathBuf> {
    files.sort_by(|(_, a), (_, b)| b.cmp(a));
    files
        .into_iter()
        .skip(keep)
        .filter(|(_, modified)| {
            now.duration_since(*modified)
                .is_ok_and(|age| age >= PRUNE_AFTER)
        })
        .map(|(path, _)| path)
        .collect()
}

/// Replaces the summary file in one step, so a reader never sees a half-written file.
fn write_summary(path: &Path, summary: &TimingsSummary) -> io::Result<()> {
    let partial = path.with_extension("partial");
    fs::write(&partial, serde_json::to_vec_pretty(summary)?)?;
    fs::rename(partial, path)
}

#[cfg(test)]
mod tests {
    use mirrord_protocol::file::{AccessFileRequest, ReadFileRequest, XstatRequest, XstatResponse};

    use super::*;

    fn label() -> SessionLabel {
        SessionLabel {
            session_id: "session".to_owned(),
            target: "deployment/app".to_owned(),
            mirrord_version: "0.0.0".to_owned(),
            started_at: "2026-11-25T10:00:00Z".to_owned(),
            operator: true,
            kube_context: None,
            namespace: None,
            baggage: None,
        }
    }

    fn snapshot(kinds: Vec<KindTimings>) -> CallTimingsSnapshot {
        CallTimingsSnapshot {
            kinds,
            slowest: Vec::new(),
        }
    }

    fn slow(subject: &str, nanos: u64) -> SlowCall {
        SlowCall {
            kind: CallKind::NameLookup,
            subject: Some(subject.to_owned()),
            nanos,
        }
    }

    fn timings(kind: CallKind, calls: u64, total_nanos: u64, slowest_nanos: u64) -> KindTimings {
        KindTimings {
            kind,
            calls,
            total_nanos,
            slowest_nanos,
        }
    }

    fn stat(path: &str) -> FileRequest {
        FileRequest::Xstat(XstatRequest {
            path: Some(path.into()),
            fd: None,
            follow_symlink: true,
        })
    }

    fn not_found() -> ProxyToLayerMessage {
        ProxyToLayerMessage::File(FileResponse::Xstat(Err(ResponseError::RemoteIO(
            RemoteIOError {
                raw_os_error: Some(2),
                kind: ErrorKindInternal::NotFound,
            },
        ))))
    }

    fn found() -> ProxyToLayerMessage {
        ProxyToLayerMessage::File(FileResponse::Xstat(Ok(XstatResponse {
            metadata: Default::default(),
        })))
    }

    /// A layer's snapshot holds its totals since the process started. Adding every snapshot up
    /// would count the early calls of a long-running process again and again.
    #[test]
    fn a_newer_snapshot_replaces_the_layers_previous_one() {
        let mut session = SessionTimings::new(label());
        session.layer_snapshot(
            LayerId(1),
            snapshot(vec![timings(CallKind::NameLookup, 1, 100, 100)]),
        );
        session.layer_snapshot(
            LayerId(1),
            snapshot(vec![timings(CallKind::NameLookup, 3, 450, 300)]),
        );

        assert_eq!(
            session.summary().kinds,
            [KindSummary {
                kind: "name_lookup".to_owned(),
                calls: 3,
                total_nanos: 450,
                slowest_nanos: 300,
            }]
        );
    }

    /// The session's numbers are every process added together: a process that has exited still
    /// counts, calls and totals add up, and the slowest call is the slowest of any process.
    #[test]
    fn the_summary_adds_up_live_and_closed_layers_slowest_kind_first() {
        let mut session = SessionTimings::new(label());
        session.layer_snapshot(
            LayerId(1),
            snapshot(vec![
                timings(CallKind::FileStat, 10, 1_000, 400),
                timings(CallKind::NameLookup, 1, 5_000, 5_000),
            ]),
        );
        session.layer_closed(LayerId(1));
        session.layer_snapshot(
            LayerId(2),
            snapshot(vec![timings(CallKind::FileStat, 5, 2_000, 900)]),
        );

        let kinds = session
            .summary()
            .kinds
            .into_iter()
            .map(|kind| (kind.kind, kind.calls, kind.total_nanos, kind.slowest_nanos))
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                ("name_lookup".to_owned(), 1, 5_000, 5_000),
                ("file_stat".to_owned(), 15, 3_000, 900),
            ]
        );
    }

    /// Every process keeps its own short list. The session's list is the slowest of all of them,
    /// and a process that has exited keeps its place in it.
    #[test]
    fn the_slowest_calls_are_the_slowest_of_every_layer_live_or_closed() {
        let mut session = SessionTimings::new(label());
        let kinds = vec![timings(CallKind::NameLookup, 1, 1, 1)];
        session.layer_snapshot(
            LayerId(1),
            CallTimingsSnapshot {
                kinds: kinds.clone(),
                slowest: (0..MAX_SLOW_CALLS as u64)
                    .map(|index| slow("closed", 1_000 + index))
                    .collect(),
            },
        );
        session.layer_closed(LayerId(1));
        session.layer_snapshot(
            LayerId(2),
            CallTimingsSnapshot {
                kinds,
                slowest: vec![slow("live and slowest", 9_000), slow("live and fast", 10)],
            },
        );

        let slowest = session.summary().slowest_calls;

        assert_eq!(slowest.len(), MAX_SLOW_CALLS);
        assert_eq!(
            slowest
                .first()
                .map(|call| (call.subject.as_deref(), call.nanos)),
            Some((Some("live and slowest"), 9_000))
        );
        assert!(
            slowest
                .iter()
                .all(|call| call.subject.as_deref() != Some("live and fast")),
            "the list is full of slower calls"
        );
        assert_eq!(
            slowest.last().map(|call| call.nanos),
            Some(1_001),
            "the fastest call of the closed layer made room for the live one"
        );
    }

    #[test]
    fn path_lookups_are_counted_under_their_directory_with_their_outcome() {
        let mut session = SessionTimings::new(label());
        let start = Instant::now();
        let layer = LayerId(1);

        session.file_request(layer, 1, &stat("/app/config/settings.yaml"), start);
        session.layer_response(layer, 1, &not_found(), start + Duration::from_millis(3));
        session.file_request(layer, 2, &stat("/app/config/local.yaml"), start);
        session.layer_response(layer, 2, &found(), start + Duration::from_millis(2));

        let access = FileRequest::Access(AccessFileRequest {
            pathname: "/data/cache".into(),
            mode: 0,
        });
        session.file_request(layer, 3, &access, start);
        session.layer_response(layer, 3, &not_found(), start + Duration::from_millis(1));

        // A read has no path, so its answer is not a lookup.
        let read = FileRequest::Read(ReadFileRequest {
            remote_fd: 4,
            buffer_size: 16,
        });
        session.file_request(layer, 4, &read, start);
        session.layer_response(layer, 4, &found(), start);

        assert_eq!(
            session.summary().path_lookups,
            [
                DirLookups {
                    dir: "/app/config".to_owned(),
                    lookups: 2,
                    not_found: 1,
                    total_nanos: 5_000_000,
                },
                DirLookups {
                    dir: "/data".to_owned(),
                    lookups: 1,
                    not_found: 1,
                    total_nanos: 1_000_000,
                },
            ]
        );
    }

    /// A process that dies with a lookup in flight never gets its answer. Its entry must go with
    /// it, or a session with many short processes grows the map for as long as it runs.
    #[test]
    fn a_closed_layers_unanswered_lookups_are_forgotten() {
        let mut session = SessionTimings::new(label());
        let now = Instant::now();

        session.file_request(LayerId(1), 1, &stat("/app/a"), now);
        session.file_request(LayerId(2), 1, &stat("/app/b"), now);
        session.layer_closed(LayerId(1));

        assert_eq!(session.pending_lookups.len(), 1);
        assert!(session.pending_lookups.contains_key(&(LayerId(2), 1)));
    }

    #[test]
    fn lookups_past_the_directory_cap_are_counted_as_untracked() {
        let mut session = SessionTimings::new(label());
        let now = Instant::now();
        let layer = LayerId(1);

        for index in 0..=MAX_TRACKED_DIRS {
            let message_id = index as MessageId;
            session.file_request(layer, message_id, &stat(&format!("/dir{index}/file")), now);
            session.layer_response(layer, message_id, &not_found(), now);
        }
        // A directory that is already tracked keeps counting after the cap is reached.
        session.file_request(layer, 1_000, &stat("/dir0/other"), now);
        session.layer_response(layer, 1_000, &not_found(), now);

        let summary = session.summary();
        assert_eq!(summary.path_lookups.len(), MAX_TRACKED_DIRS);
        assert_eq!(summary.untracked_path_lookups, 1);
        let dir0 = summary
            .path_lookups
            .iter()
            .find(|lookups| lookups.dir == "/dir0")
            .expect("the first directory is tracked");
        assert_eq!(dir0.lookups, 2);
    }

    #[test]
    fn a_path_is_counted_under_its_parent_cut_to_the_key_depth() {
        let key = |path: &str| dir_key(Path::new(path));

        assert_eq!(
            key("/app/config/settings.yaml").as_deref(),
            Some("/app/config")
        );
        assert_eq!(key("/settings.yaml").as_deref(), Some("/"));
        assert_eq!(key("/").as_deref(), Some("/"));
        assert_eq!(key("/a//b/c/").as_deref(), Some("/a/b"));
        assert_eq!(
            key("/app/vendor/bundle/ruby/3.3.0/gems/rack/lib/rack.rb").as_deref(),
            Some("/app/vendor/bundle/ruby"),
        );
        assert_eq!(key("relative/settings.yaml"), None);
    }

    /// A dozen sessions running side by side each start a proxy that tidies the directory. The
    /// file of a session that is still running must survive that, so only files that are both
    /// beyond the newest few and old are removed.
    #[test]
    fn only_old_summaries_beyond_the_newest_are_removed() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 24 * 60 * 60);
        let ago = |age: Duration| now - age;
        let hour = Duration::from_secs(60 * 60);
        let files = vec![
            (PathBuf::from("last week"), ago(7 * 24 * hour)),
            (PathBuf::from("a minute ago"), ago(Duration::from_secs(60))),
            (PathBuf::from("two days ago"), ago(48 * hour)),
            (PathBuf::from("an hour ago"), ago(hour)),
            (PathBuf::from("three days ago"), ago(72 * hour)),
        ];

        assert_eq!(
            stale_summaries(files.clone(), 0, now),
            [
                PathBuf::from("two days ago"),
                PathBuf::from("three days ago"),
                PathBuf::from("last week")
            ],
            "the two recent files stay even with no room left"
        );
        assert_eq!(
            stale_summaries(files.clone(), 3, now),
            [PathBuf::from("three days ago"), PathBuf::from("last week")],
            "the newest three stay whatever their age"
        );
        assert!(stale_summaries(files, 5, now).is_empty());
    }

    /// The recorder end to end: a published summary reaches the session's file, a later one
    /// replaces it, and the oldest of too many other sessions' files is removed.
    #[tokio::test]
    async fn the_recorder_writes_the_latest_summary_and_prunes_old_sessions() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..KEPT_SESSIONS {
            let path = dir.path().join(format!("other{index}.{SUMMARY_EXTENSION}"));
            fs::write(&path, "{}").unwrap();
            let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 + index as u64);
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }

        let mut recorder = CallTimingsRecorder::start(dir.path().to_owned(), label());
        recorder.timings().layer_snapshot(
            LayerId(1),
            snapshot(vec![timings(CallKind::NameLookup, 1, 100, 100)]),
        );
        recorder.publish();
        recorder.timings().layer_snapshot(
            LayerId(1),
            snapshot(vec![timings(CallKind::NameLookup, 2, 300, 200)]),
        );
        recorder.publish();
        let expected = recorder.timings().summary();
        recorder.finish().await;

        let written = fs::read(dir.path().join(format!("session.{SUMMARY_EXTENSION}"))).unwrap();
        let written: TimingsSummary = serde_json::from_slice(&written).unwrap();
        assert_eq!(written, expected);

        let mut left = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        left.sort();
        assert_eq!(left.len(), KEPT_SESSIONS, "{left:?}");
        assert!(
            !left.contains(&format!("other0.{SUMMARY_EXTENSION}")),
            "the session with the oldest file made room for this one: {left:?}"
        );
    }
}
