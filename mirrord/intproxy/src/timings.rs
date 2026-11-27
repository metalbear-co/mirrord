//! The session's summary of what the application waited on mirrord for, kept in a small file
//! that `mirrord diagnose timings` reads.
//!
//! The layers measure their own remote calls and send the totals as
//! [`CallTimingsSnapshot`]s. A process can be killed at any moment and `mirrord exec` leaves no
//! CLI process behind, so the internal proxy, which outlives every layer of its session, is the
//! one that keeps the numbers: [`SessionTimings`] adds the layers up and [`CallTimingsRecorder`]
//! writes the result to `<timings directory>/<session id>.json`.
//!
//! A layer only sends a snapshot together with a request it makes anyway, so an application
//! that makes a few calls and then sits idle keeps its last calls to itself. The proxy sees
//! every request and its answer too, so it keeps its own count per layer and the summary
//! takes the larger of the two for every number of a kind. Both sides only count up, and the
//! application waited at least as long as either measured, so what is shown never goes down.
//!
//! The proxy also sees every file request with its path, which the layers' counters leave out to
//! stay cheap. It tallies path lookups per directory and how many of them the cluster answered
//! with "not found", which is what the command's suggestions are built on.

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime},
};

use mirrord_intproxy_protocol::{
    CallKind, CallTimingsSnapshot, KindTimings, LayerId, LayerToProxyMessage, MAX_SLOW_CALLS,
    MessageId, ProxyToLayerMessage, SlowCall,
};
use mirrord_protocol::{
    ErrorKindInternal, FileRequest, FileResponse, RemoteIOError, ResponseError,
};
use serde::{Deserialize, Serialize};

use crate::main_tasks::{FromLayer, ProxyMessage};
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
};

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
pub(crate) const WRITE_INTERVAL: Duration = Duration::from_secs(1);

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
    /// The kube context the session's client resolved when its internal proxy started: the
    /// configured one, or the kubeconfig's current one. The command uses it to ask the same
    /// cluster about the session after the current context has moved on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kube_context: Option<String>,
    /// The kubeconfig the session was started with, when the configuration or `KUBECONFIG`
    /// names one, as it was written there. A relative path in it is read against the directory
    /// the command runs in. It names the user's home directory, so reports leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kubeconfig: Option<String>,
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
    /// Lookups that are missing from `path_lookups`: [`MAX_TRACKED_DIRS`] directories were
    /// already tracked, or the path does not say which directory it is in (it is relative, as
    /// one given next to an open directory is). When this is not zero, `path_lookups` is not
    /// the whole picture.
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
    fn add(&mut self, other: Self) {
        self.calls = self.calls.saturating_add(other.calls);
        self.total_nanos = self.total_nanos.saturating_add(other.total_nanos);
        self.slowest_nanos = self.slowest_nanos.max(other.slowest_nanos);
    }

    fn add_call(&mut self, nanos: u64) {
        self.add(Self {
            calls: 1,
            total_nanos: nanos,
            slowest_nanos: nanos,
        });
    }
}

impl From<&KindTimings> for KindTotals {
    fn from(timings: &KindTimings) -> Self {
        Self {
            calls: timings.calls,
            total_nanos: timings.total_nanos,
            slowest_nanos: timings.slowest_nanos,
        }
    }
}

/// One layer's totals per kind, from its own latest snapshot and from what the proxy saw it wait
/// for.
///
/// Every number is the larger of the two sides. The proxy has always answered at least as many
/// calls as the layer has reported, and the layer's waits are the application's real ones, a
/// little longer than what the proxy measures between a request arriving and its answer leaving.
/// Taking either side whole would make a total jump down when the other side takes over, for
/// example when one more call reaches the proxy before the layer's next snapshot. The larger of
/// each never goes down, and the application waited at least that long.
fn layer_totals(
    snapshot: Option<&CallTimingsSnapshot>,
    seen_by_proxy: Option<&HashMap<CallKind, KindTotals>>,
) -> HashMap<CallKind, KindTotals> {
    let mut totals = seen_by_proxy.cloned().unwrap_or_default();
    for timings in snapshot.into_iter().flat_map(|snapshot| &snapshot.kinds) {
        let reported = KindTotals::from(timings);
        let seen = totals.entry(timings.kind).or_default();
        *seen = KindTotals {
            calls: seen.calls.max(reported.calls),
            total_nanos: seen.total_nanos.max(reported.total_nanos),
            slowest_nanos: seen.slowest_nanos.max(reported.slowest_nanos),
        };
    }

    totals
}

/// Takes every call timings snapshot of `layer_id` out of the messages that wait for a
/// reconnect to end, and returns the last one.
///
/// For a layer that has just closed. Left in the queue, its snapshot would be replayed after the
/// layer's totals were added to the closed layers, and would then count a second time as a live
/// layer that never closes.
pub(crate) fn take_queued_snapshots(
    queue: &mut VecDeque<ProxyMessage>,
    layer_id: LayerId,
) -> Option<CallTimingsSnapshot> {
    let mut last = None;
    queue.retain_mut(|message| match message {
        ProxyMessage::FromLayer(FromLayer {
            layer_id: sender,
            message: LayerToProxyMessage::CallTimings(snapshot),
            ..
        }) if *sender == layer_id => {
            last = Some(std::mem::take(snapshot));
            false
        }
        _ => true,
    });

    last
}

#[derive(Default)]
struct DirTally {
    lookups: u64,
    not_found: u64,
    total_nanos: u64,
}

/// A call the proxy has not answered yet.
struct PendingCall {
    kind: CallKind,
    received: Instant,
    /// Where the call's path lookup is counted, when it is such a call.
    lookup: Option<PathLookup>,
}

/// Where a path lookup is counted.
enum PathLookup {
    /// Under this directory.
    In(String),
    /// Under no directory, because the path does not say which one it is in.
    Unplaced,
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
    /// What the proxy itself saw each connected layer wait for.
    seen_by_proxy: HashMap<LayerId, HashMap<CallKind, KindTotals>>,
    /// Calls the proxy has not answered yet.
    pending_calls: HashMap<(LayerId, MessageId), PendingCall>,
    /// Whether the numbers moved since the summary was last handed out for writing.
    changed: bool,
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
            seen_by_proxy: HashMap::new(),
            pending_calls: HashMap::new(),
            changed: false,
            dirs: BTreeMap::new(),
            untracked_path_lookups: 0,
        }
    }

    pub fn layer_snapshot(&mut self, layer_id: LayerId, snapshot: CallTimingsSnapshot) {
        self.live_layers.insert(layer_id, snapshot);
        self.changed = true;
    }

    /// Keeps the numbers of a layer whose process is gone, and forgets its unanswered calls.
    pub fn layer_closed(&mut self, layer_id: LayerId) {
        let snapshot = self.live_layers.remove(&layer_id);
        let seen_by_proxy = self.seen_by_proxy.remove(&layer_id);
        for (kind, totals) in layer_totals(snapshot.as_ref(), seen_by_proxy.as_ref()) {
            self.closed_layers.entry(kind).or_default().add(totals);
        }
        if let Some(snapshot) = snapshot {
            self.closed_slowest.extend(snapshot.slowest);
            keep_slowest(&mut self.closed_slowest);
        }
        self.pending_calls
            .retain(|(pending_layer, _), _| *pending_layer != layer_id);
        self.changed = true;
    }

    /// Remembers a call the application waits on the cluster for until
    /// [`Self::layer_response`] sees its answer.
    pub fn layer_request(
        &mut self,
        layer_id: LayerId,
        message_id: MessageId,
        request: &LayerToProxyMessage,
        now: Instant,
    ) {
        let Some(kind) = CallKind::of(request) else {
            return;
        };
        let lookup = match request {
            LayerToProxyMessage::File(request) => looked_up_path(request)
                .map(|path| dir_key(path).map_or(PathLookup::Unplaced, PathLookup::In)),
            _ => None,
        };

        self.pending_calls.insert(
            (layer_id, message_id),
            PendingCall {
                kind,
                received: now,
                lookup,
            },
        );
    }

    /// Counts the answer to a call, and a path lookup under its directory.
    pub fn layer_response(
        &mut self,
        layer_id: LayerId,
        message_id: MessageId,
        response: &ProxyToLayerMessage,
        now: Instant,
    ) {
        let Some(call) = self.pending_calls.remove(&(layer_id, message_id)) else {
            return;
        };

        let waited = now.saturating_duration_since(call.received);
        let waited = u64::try_from(waited.as_nanos()).unwrap_or(u64::MAX);
        self.seen_by_proxy
            .entry(layer_id)
            .or_default()
            .entry(call.kind)
            .or_default()
            .add_call(waited);
        self.changed = true;

        let dir = match call.lookup {
            Some(PathLookup::In(dir)) => dir,
            Some(PathLookup::Unplaced) => {
                self.untracked_path_lookups = self.untracked_path_lookups.saturating_add(1);
                return;
            }
            None => return,
        };

        if !self.dirs.contains_key(&dir) && self.dirs.len() >= MAX_TRACKED_DIRS {
            if self.untracked_path_lookups == 0 {
                tracing::debug!(
                    tracked = MAX_TRACKED_DIRS,
                    dir,
                    "Path lookups in more directories than the timings summary tracks, \
                    the rest are only counted."
                );
            }
            self.untracked_path_lookups = self.untracked_path_lookups.saturating_add(1);
            return;
        }

        let tally = self.dirs.entry(dir).or_default();
        tally.lookups = tally.lookups.saturating_add(1);
        tally.not_found = tally
            .not_found
            .saturating_add(u64::from(is_not_found(response)));
        tally.total_nanos = tally.total_nanos.saturating_add(waited);
    }

    pub fn summary(&self) -> TimingsSummary {
        let mut totals = self.closed_layers.clone();
        let connected = self
            .live_layers
            .keys()
            .chain(self.seen_by_proxy.keys())
            .collect::<HashSet<_>>();
        for layer_id in connected {
            let layer = layer_totals(
                self.live_layers.get(layer_id),
                self.seen_by_proxy.get(layer_id),
            );
            for (kind, layer) in layer {
                totals.entry(kind).or_default().add(layer);
            }
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
///
/// A path relative to an open directory is a lookup too. Leaving it out would let a file found
/// that way go unseen, and its directory would look like one where nothing is ever found.
fn looked_up_path(request: &FileRequest) -> Option<&Path> {
    match request {
        FileRequest::Open(request) => Some(&request.path),
        FileRequest::OpenRelative(request) => Some(&request.path),
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

/// One write of the summary file, for the thread that does them.
struct WriteJob {
    summary: TimingsSummary,
    /// Whether the directory still has to be made and tidied.
    first_write: bool,
    /// Where the file thread sends how the write went, for the writer task.
    done_tx: oneshot::Sender<io::Result<()>>,
}

/// Keeps a session's [`SessionTimings`] and writes its summary file.
///
/// Writing happens off the proxy's main loop, because the file is in the user's home directory
/// and a slow disk there must not hold up the application's traffic. A task decides when to
/// write and a thread of its own does the writing. The thread is a plain one and not one of the
/// runtime's blocking threads, because the runtime waits for those when the proxy exits, and a
/// home directory that stopped answering would then keep the proxy alive.
pub struct CallTimingsRecorder {
    timings: SessionTimings,
    /// The proxy sends the newest summary; the writer task writes the last one it finds.
    summary_tx: watch::Sender<Option<TimingsSummary>>,
    /// The proxy sends a tick to end the writer task's pause between two writes. Dropping it
    /// does the same.
    write_now_tx: watch::Sender<()>,
    writer: JoinHandle<()>,
}

impl CallTimingsRecorder {
    /// Starts the task and the thread that write the session's summary into `dir`. Must be
    /// called from a tokio runtime.
    ///
    /// # Returns
    ///
    /// [`None`] for a session whose id is not made of ASCII letters, digits, `-` and `_`. The
    /// id can come from the `MIRRORD_SESSION_ID` environment variable, and one with a path in
    /// it would put the summary, and the cleanup of old summaries next to it, outside `dir`.
    /// The ids mirrord makes itself are hex numbers and UUIDs. Also [`None`] when the thread
    /// cannot be started.
    pub fn start(dir: PathBuf, session: SessionLabel) -> Option<Self> {
        Self::start_with(dir, session, |path, summary, first_write| {
            if first_write {
                prepare_dir(path)?;
            }
            write_summary(path, summary)
        })
    }

    /// [`Self::start`] with the function that puts a summary into its file, which a test
    /// replaces to stand in for a disk that does not answer.
    fn start_with(
        dir: PathBuf,
        session: SessionLabel,
        mut write: impl FnMut(&Path, &TimingsSummary, bool) -> io::Result<()> + Send + 'static,
    ) -> Option<Self> {
        let is_file_name = !session.session_id.is_empty()
            && session.session_id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            });
        if !is_file_name {
            tracing::debug!(
                session_id = session.session_id,
                "The session id cannot be a file name, the session's call timings are not recorded."
            );
            return None;
        }

        let path = dir.join(format!("{}.{SUMMARY_EXTENSION}", session.session_id));
        // The writer task sends one job per write; the file thread does them in order and ends
        // when the task is gone.
        let (job_tx, job_rx) = mpsc::channel::<WriteJob>();
        let file_thread = thread::Builder::new()
            .name("mirrord-call-timings".to_owned())
            .spawn(move || {
                for job in job_rx {
                    let result = write(&path, &job.summary, job.first_write);
                    // The writer task stops waiting when the proxy exits.
                    let _ = job.done_tx.send(result);
                }
            });
        if let Err(error) = file_thread {
            tracing::debug!(%error, "Could not start the thread that writes the call timings.");
            return None;
        }

        let (summary_tx, summary_rx) = watch::channel(None);
        let (write_now_tx, write_now_rx) = watch::channel(());
        let writer = tokio::spawn(write_summaries(job_tx, summary_rx, write_now_rx));

        Some(Self {
            timings: SessionTimings::new(session),
            summary_tx,
            write_now_tx,
            writer,
        })
    }

    pub fn timings(&mut self) -> &mut SessionTimings {
        &mut self.timings
    }

    /// Hands the current summary to the writer task, which writes it once [`WRITE_INTERVAL`] has
    /// passed since its last write. Only the newest summary is kept when the writer is behind.
    pub fn publish(&mut self) {
        self.timings.changed = false;
        self.summary_tx.send_replace(Some(self.timings.summary()));
    }

    /// [`Self::publish`], when the numbers moved since the last one.
    ///
    /// The proxy calls this on a timer, so the file follows the application within about
    /// [`WRITE_INTERVAL`] without a summary being built for every single call.
    pub fn publish_if_changed(&mut self) {
        if self.timings.changed {
            self.publish();
        }
    }

    /// Like [`Self::publish`], but the summary is written right away.
    ///
    /// For when a process of the session has just exited: that is the moment a person runs
    /// `mirrord diagnose timings`, and the file must already hold the process's last numbers.
    pub fn publish_now(&mut self) {
        self.publish();
        self.write_now_tx.send_replace(());
    }

    /// Waits for the last summary to reach the file, for at most [`FINAL_WRITE_TIMEOUT`].
    ///
    /// A write that is still running after that is left behind on its thread. Nothing waits for
    /// that thread, so it ends with the process.
    pub async fn finish(self) {
        let Self {
            summary_tx,
            write_now_tx,
            mut writer,
            ..
        } = self;
        drop(summary_tx);
        drop(write_now_tx);

        if tokio::time::timeout(FINAL_WRITE_TIMEOUT, &mut writer)
            .await
            .is_err()
        {
            writer.abort();
            tracing::warn!(
                timeout = ?FINAL_WRITE_TIMEOUT,
                "The last call timings summary was not written in time, \
                the file keeps the previous one."
            );
        }
    }
}

/// Hands every new summary to the file thread until the recorder is dropped, pausing for
/// [`WRITE_INTERVAL`] after each write unless `write_now_rx` says to write now.
///
/// Stops at the first failed write: a directory that cannot be written now will not be writable
/// a second later, and the summary is not worth a warning every second.
async fn write_summaries(
    job_tx: mpsc::Sender<WriteJob>,
    mut summary_rx: watch::Receiver<Option<TimingsSummary>>,
    mut write_now_rx: watch::Receiver<()>,
) {
    let mut first_write = true;

    while summary_rx.changed().await.is_ok() {
        let Some(summary) = summary_rx.borrow_and_update().clone() else {
            continue;
        };

        // The file thread sends how this one write went.
        let (done_tx, done_rx) = oneshot::channel();
        let job = WriteJob {
            summary,
            first_write,
            done_tx,
        };
        let written = match job_tx.send(job) {
            Ok(()) => done_rx.await.ok(),
            Err(_) => None,
        };
        match written {
            Some(Ok(())) => first_write = false,
            Some(Err(error)) => {
                tracing::warn!(
                    %error,
                    "Failed to write the call timings summary, \
                    `mirrord diagnose timings` will not show this session."
                );
                return;
            }
            None => {
                tracing::warn!("The thread writing the call timings summary is gone.");
                return;
            }
        }

        // A dropped recorder also ends the pause, so the last summary is written at once and
        // the loop then ends on the closed `summary_rx` channel.
        tokio::select! {
            _ = tokio::time::sleep(WRITE_INTERVAL) => {}
            _ = write_now_rx.changed() => {}
        }
    }
}

/// Creates the timings directory and removes the summaries that are both beyond the
/// [`KEPT_SESSIONS`] newest (counting this session's file) and older than [`PRUNE_AFTER`].
///
/// On unix the directory is set to the user alone, also one that was already there: summaries
/// name the paths and hosts the application asked for.
fn prepare_dir(summary_path: &Path) -> io::Result<()> {
    let Some(dir) = summary_path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;

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
///
/// On unix the file is created for the user alone. A partial file left by a proxy that died is
/// removed first, because a mode only applies to a file that is being created.
fn write_summary(path: &Path, summary: &TimingsSummary) -> io::Result<()> {
    let partial = path.with_extension("partial");
    match fs::remove_file(&partial) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(&partial)?
        .write_all(&serde_json::to_vec_pretty(summary)?)?;
    fs::rename(partial, path)
}

#[cfg(test)]
mod tests {
    use mirrord_protocol::file::{
        AccessFileRequest, OpenFileResponse, OpenRelativeFileRequest, ReadFileRequest,
        XstatRequest, XstatResponse,
    };

    use super::*;

    fn label() -> SessionLabel {
        SessionLabel {
            session_id: "session".to_owned(),
            target: "deployment/app".to_owned(),
            mirrord_version: "0.0.0".to_owned(),
            started_at: "2026-11-25T10:00:00Z".to_owned(),
            operator: true,
            kube_context: None,
            kubeconfig: None,
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

    fn stat(path: &str) -> LayerToProxyMessage {
        LayerToProxyMessage::File(FileRequest::Xstat(XstatRequest {
            path: Some(path.into()),
            fd: None,
            follow_symlink: true,
        }))
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

    /// On a cluster a millisecond away, an application's first calls are over before its layer
    /// is due to send anything, and an application that then waits for traffic sends nothing
    /// more. CI showed exactly that: a session whose application had read a file had an empty
    /// summary for as long as it ran. The proxy answered those calls, so it counts them itself.
    #[test]
    fn calls_the_layer_has_not_reported_yet_are_counted_by_the_proxy() {
        let mut session = SessionTimings::new(label());
        let start = Instant::now();
        let layer = LayerId(1);

        session.layer_request(layer, 1, &stat("/etc/hostname"), start);
        session.layer_response(layer, 1, &found(), start + Duration::from_millis(2));
        session.layer_request(layer, 2, &stat("/etc/hosts"), start);
        session.layer_response(layer, 2, &found(), start + Duration::from_millis(5));

        assert_eq!(
            session.summary().kinds,
            [KindSummary {
                kind: "file_stat".to_owned(),
                calls: 2,
                total_nanos: 7_000_000,
                slowest_nanos: 5_000_000,
            }]
        );
    }

    /// The layer and the proxy count the same calls, so a kind is counted once, not once per
    /// side. While the layer's snapshot is behind, the proxy's count of calls stands; once it
    /// has caught up, its waits, which are the application's real ones and a little longer than
    /// the proxy's, are what shows.
    #[test]
    fn a_kind_counted_by_both_sides_is_counted_once_with_the_larger_numbers() {
        let mut session = SessionTimings::new(label());
        let start = Instant::now();
        let layer = LayerId(1);
        for message_id in 1..=3 {
            session.layer_request(layer, message_id, &stat("/etc/hostname"), start);
            session.layer_response(
                layer,
                message_id,
                &found(),
                start + Duration::from_millis(1),
            );
        }

        session.layer_snapshot(
            layer,
            snapshot(vec![timings(CallKind::FileStat, 2, 2_400_000, 1_300_000)]),
        );
        assert_eq!(
            session.summary().kinds,
            [KindSummary {
                kind: "file_stat".to_owned(),
                calls: 3,
                total_nanos: 3_000_000,
                slowest_nanos: 1_300_000,
            }],
            "the snapshot has seen two of the three calls the proxy answered"
        );

        session.layer_snapshot(
            layer,
            snapshot(vec![timings(CallKind::FileStat, 3, 3_600_000, 1_300_000)]),
        );
        let caught_up = [KindSummary {
            kind: "file_stat".to_owned(),
            calls: 3,
            total_nanos: 3_600_000,
            slowest_nanos: 1_300_000,
        }];
        assert_eq!(session.summary().kinds, caught_up);

        session.layer_closed(layer);
        assert_eq!(
            session.summary().kinds,
            caught_up,
            "a closed layer keeps the same numbers"
        );
    }

    /// The application can wait far longer for a call than the proxy takes to answer it, for
    /// example behind another thread that is writing to the same connection. When the summary
    /// took the proxy's numbers whole as soon as the proxy had seen one more call than the
    /// layer's snapshot, a wait of 100 ms the layer had reported showed as 2 ms until the next
    /// snapshot. A number that was shown must not go down.
    #[test]
    fn a_wait_the_layer_reported_does_not_shrink_when_the_proxy_sees_one_more_call() {
        let mut session = SessionTimings::new(label());
        let start = Instant::now();
        let layer = LayerId(1);

        session.layer_request(layer, 1, &stat("/etc/hostname"), start);
        session.layer_response(layer, 1, &found(), start + Duration::from_millis(1));
        session.layer_snapshot(
            layer,
            snapshot(vec![timings(
                CallKind::FileStat,
                1,
                100_000_000,
                100_000_000,
            )]),
        );
        session.layer_request(layer, 2, &stat("/etc/hosts"), start);
        session.layer_response(layer, 2, &found(), start + Duration::from_millis(1));

        assert_eq!(
            session.summary().kinds,
            [KindSummary {
                kind: "file_stat".to_owned(),
                calls: 2,
                total_nanos: 100_000_000,
                slowest_nanos: 100_000_000,
            }]
        );
    }

    /// While the proxy reconnects to the agent it queues what the layers send, snapshots
    /// included, and replays the queue afterwards. A layer that closes in the meantime has its
    /// totals added to the closed layers at once. Its queued snapshot, replayed later, then
    /// came back as a live layer that never closes, and every summary counted its calls twice.
    #[test]
    fn a_snapshot_queued_during_a_reconnect_counts_once_when_its_layer_closes() {
        let queued = |layer_id: u64, calls: u64| {
            ProxyMessage::FromLayer(FromLayer {
                message_id: calls,
                layer_id: LayerId(layer_id),
                message: LayerToProxyMessage::CallTimings(snapshot(vec![timings(
                    CallKind::NameLookup,
                    calls,
                    calls * 10,
                    10,
                )])),
            })
        };
        let mut queue = VecDeque::from([
            queued(1, 2),
            ProxyMessage::FromLayer(FromLayer {
                message_id: 9,
                layer_id: LayerId(1),
                message: stat("/etc/hostname"),
            }),
            queued(2, 7),
            queued(1, 5),
        ]);
        let mut session = SessionTimings::new(label());

        let last = take_queued_snapshots(&mut queue, LayerId(1)).expect("two were queued");
        session.layer_snapshot(LayerId(1), last);
        session.layer_closed(LayerId(1));

        assert_eq!(
            session
                .summary()
                .kinds
                .iter()
                .map(|kind| kind.calls)
                .collect::<Vec<_>>(),
            [5],
            "the layer's last snapshot, once"
        );
        let left = queue
            .iter()
            .map(|message| match message {
                ProxyMessage::FromLayer(from_layer) => (
                    from_layer.layer_id,
                    matches!(from_layer.message, LayerToProxyMessage::CallTimings(_)),
                ),
                other => panic!("unexpected message in the queue: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            left,
            [(LayerId(1), false), (LayerId(2), true)],
            "nothing of the closed layer's timings is left to replay, the rest is untouched"
        );
        assert_eq!(take_queued_snapshots(&mut queue, LayerId(3)), None);
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

        session.layer_request(layer, 1, &stat("/app/config/settings.yaml"), start);
        session.layer_response(layer, 1, &not_found(), start + Duration::from_millis(3));
        session.layer_request(layer, 2, &stat("/app/config/local.yaml"), start);
        session.layer_response(layer, 2, &found(), start + Duration::from_millis(2));

        let access = LayerToProxyMessage::File(FileRequest::Access(AccessFileRequest {
            pathname: "/data/cache".into(),
            mode: 0,
        }));
        session.layer_request(layer, 3, &access, start);
        session.layer_response(layer, 3, &not_found(), start + Duration::from_millis(1));

        // A read has no path, so its answer is not a lookup.
        let read = LayerToProxyMessage::File(FileRequest::Read(ReadFileRequest {
            remote_fd: 4,
            buffer_size: 16,
        }));
        session.layer_request(layer, 4, &read, start);
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

        session.layer_request(LayerId(1), 1, &stat("/app/a"), now);
        session.layer_request(LayerId(2), 1, &stat("/app/b"), now);
        session.layer_closed(LayerId(1));

        assert_eq!(session.pending_calls.len(), 1);
        assert!(session.pending_calls.contains_key(&(LayerId(2), 1)));
    }

    #[test]
    fn lookups_past_the_directory_cap_are_counted_as_untracked() {
        let mut session = SessionTimings::new(label());
        let now = Instant::now();
        let layer = LayerId(1);

        for index in 0..=MAX_TRACKED_DIRS {
            let message_id = index as MessageId;
            session.layer_request(layer, message_id, &stat(&format!("/dir{index}/file")), now);
            session.layer_response(layer, message_id, &not_found(), now);
        }
        // A directory that is already tracked keeps counting after the cap is reached.
        session.layer_request(layer, 1_000, &stat("/dir0/other"), now);
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

    /// A file opened by a path relative to a directory the application has open belongs to no
    /// directory of the tally. Counting it as untracked is what keeps that directory from
    /// looking like one where nothing is ever found.
    #[test]
    fn a_lookup_relative_to_an_open_directory_is_counted_as_untracked() {
        let mut session = SessionTimings::new(label());
        let now = Instant::now();
        let layer = LayerId(1);

        session.layer_request(layer, 1, &stat("/app/vendor/missing"), now);
        session.layer_response(layer, 1, &not_found(), now);
        let open_relative =
            LayerToProxyMessage::File(FileRequest::OpenRelative(OpenRelativeFileRequest {
                relative_fd: 7,
                path: "plugin.yaml".into(),
                open_options: Default::default(),
            }));
        session.layer_request(layer, 2, &open_relative, now);
        let opened = ProxyToLayerMessage::File(FileResponse::Open(Ok(OpenFileResponse { fd: 8 })));
        session.layer_response(layer, 2, &opened, now);

        let summary = session.summary();
        assert_eq!(summary.path_lookups.len(), 1);
        assert_eq!(summary.untracked_path_lookups, 1);
    }

    /// The id becomes a file name, and it can come from an environment variable.
    #[test]
    fn a_session_whose_id_is_not_a_plain_name_is_not_recorded() {
        let dir = tempfile::tempdir().unwrap();

        for id in [
            "",
            ".",
            "..",
            "../escape",
            "a/b",
            "/tmp/escape",
            "a\\b",
            "a:b",
            "a\u{1b}b",
        ] {
            let session = SessionLabel {
                session_id: id.to_owned(),
                ..label()
            };
            assert!(
                CallTimingsRecorder::start(dir.path().to_owned(), session).is_none(),
                "`{id}` is not a file name"
            );
        }
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
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

    /// The summary file is in the user's home directory, which can be a network mount that
    /// stops answering. The proxy gives the last write two seconds and then exits. When the
    /// write ran on one of the runtime's blocking threads, giving up on it was not enough: the
    /// runtime waits for those threads when it shuts down, so the proxy stayed alive for as
    /// long as the disk stayed silent.
    #[test]
    fn a_write_that_never_returns_holds_up_neither_the_last_wait_nor_the_runtime() {
        // The stand-in for the silent disk tells the test when a write started, and then waits
        // for the test to let go of `release_tx`.
        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        // The thread that plays the proxy tells the test when its runtime is gone.
        let (exited_tx, exited_rx) = mpsc::channel::<()>();

        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .start_paused(true)
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut recorder = CallTimingsRecorder::start_with(
                    PathBuf::from("/nowhere"),
                    label(),
                    move |_, _, _| {
                        let _ = started_tx.send(());
                        let _ = release_rx.recv();
                        Ok(())
                    },
                )
                .unwrap();
                recorder.publish();
                while started_rx.try_recv().is_err() {
                    tokio::task::yield_now().await;
                }

                recorder.finish().await;
            });
            drop(runtime);
            let _ = exited_tx.send(());
        });

        assert!(
            exited_rx.recv_timeout(Duration::from_secs(30)).is_ok(),
            "the proxy's runtime shut down while the write was still stuck"
        );
        drop(release_tx);
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

        let mut recorder = CallTimingsRecorder::start(dir.path().to_owned(), label())
            .expect("the label's session id is a file name");
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

        let summary_path = dir.path().join(format!("session.{SUMMARY_EXTENSION}"));
        let written = fs::read(&summary_path).unwrap();
        let written: TimingsSummary = serde_json::from_slice(&written).unwrap();
        assert_eq!(written, expected);
        #[cfg(unix)]
        {
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&summary_path), 0o600, "summaries name paths and hosts");
            assert_eq!(mode(dir.path()), 0o700);
        }

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
