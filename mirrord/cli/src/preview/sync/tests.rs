use std::os::unix::fs::PermissionsExt;
use std::{collections::BTreeMap, fs, io::Read, ops::Not, path::Path, sync::Arc, time::Duration};

use flate2::read::GzDecoder;
use futures::{SinkExt, StreamExt};
use mirrord_config::{
    LayerConfig,
    config::ConfigContext,
    target::{Target, cron_job::CronJobTarget},
};
use mirrord_operator::{
    client::{
        error::{OperatorApiError, OperatorOperation},
        preview_sync::PreviewSyncConnection,
    },
    crd::preview::{
        PreviewSyncConfig, PreviewSyncConfigError, PreviewSyncEntry, PreviewSyncPrepare,
        PreviewSyncRestart, PreviewSyncRun,
    },
    sync::{
        AppReport, AppState, ManifestPath, PrepareReport, PrepareState, PushMode, RejectReason,
        SidecarState, SupervisorReport, SyncFrame, UnpairReason,
    },
};
use mirrord_progress::ProgressTracker;
use mirrord_up::{
    PrepareConfig, RequestOrigin, RunConfig, RunType, ServiceMode, SubprocessCfg, SyncConfig,
    SyncEntry, SyncRestart, SyncRunConfig,
};
use notify::{RecursiveMode, Watcher};
use tokio::{io::DuplexStream, sync::mpsc, task::JoinHandle};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

use super::{
    Connect, HandledBy, ServiceSync, Stop, SyncActivity, SyncDriver, WATCH_DEBOUNCE,
    WATCH_MAX_DELAY,
};
use crate::error::CliError;

/// How long a test waits for a frame that must come: the debounce of `--watch`, packing and the
/// push.
const FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a test waits to see that the CLI sends nothing: the longest debounce of `--watch`,
/// and as long again for packing and sending.
const QUIET_PERIOD: Duration = WATCH_MAX_DELAY.saturating_add(WATCH_DEBOUNCE.saturating_mul(4));

/// What one entry of a push carried: each path with its content, `None` for a directory.
type Pushed = BTreeMap<String, Option<String>>;

/// A push that the operator received.
#[derive(Debug)]
struct Push {
    push_id: u64,
    mode: PushMode,

    /// Each entry's paths and contents, by entry index.
    entries: BTreeMap<u32, Pushed>,

    /// Each entry's deleted paths, by entry index, for the entries that listed any.
    deleted: BTreeMap<u32, Vec<String>>,

    /// The manifest of each entry, by entry index, for a full push.
    manifests: BTreeMap<u32, Vec<ManifestPath>>,
}

/// The operator's end of one sync connection, which a test scripts.
struct Operator {
    socket: WebSocketStream<DuplexStream>,

    /// What the volume holds after the last full push that [`Self::pair_and_apply`] applied, by
    /// entry index, as the manifest described it. The operator answers a manifest as the sidecar
    /// would: it wants every path that differs from these.
    held: BTreeMap<u32, Vec<ManifestPath>>,
}

impl Operator {
    async fn send(&mut self, frame: SyncFrame) {
        self.socket
            .send(Message::Binary(frame.encode().into()))
            .await
            .unwrap();
    }

    /// The next frame from the CLI, or `None` if none comes within `timeout`.
    async fn recv_within(&mut self, timeout: Duration) -> Option<SyncFrame> {
        let message = tokio::time::timeout(timeout, self.socket.next())
            .await
            .ok()??
            .unwrap();
        match message {
            Message::Binary(bytes) => Some(SyncFrame::decode(&bytes).unwrap()),
            message => panic!("the CLI sent {message:?}"),
        }
    }

    async fn recv(&mut self) -> SyncFrame {
        self.recv_within(FRAME_TIMEOUT)
            .await
            .expect("the CLI sent nothing in time")
    }

    /// Receives a push from its first frame (the manifest of a full push, or the `Begin` of an
    /// incremental one) to its `Commit`, and unpacks it. Answers the manifest with the paths that
    /// differ from [`Self::held`].
    async fn recv_push(&mut self) -> Push {
        let mut manifests = BTreeMap::<u32, Vec<ManifestPath>>::new();
        let (push_id, mode, plans) = loop {
            match self.recv().await {
                SyncFrame::Manifest { index, paths, .. } => {
                    manifests.entry(index).or_default().extend(paths);
                }
                SyncFrame::ManifestDone { push_id } => {
                    for (index, paths) in &manifests {
                        let held = self.held.get(index).cloned().unwrap_or_default();
                        let paths = paths
                            .iter()
                            .filter(|path| held.contains(path).not())
                            .map(|path| path.path.clone())
                            .collect();
                        self.send(SyncFrame::Want {
                            push_id,
                            index: *index,
                            paths,
                        })
                        .await;
                    }
                    self.send(SyncFrame::WantDone { push_id }).await;
                }
                SyncFrame::Begin {
                    push_id,
                    mode,
                    entries,
                } => break (push_id, mode, entries),
                frame => panic!("a push starts with `Manifest` or `Begin`, not {frame:?}"),
            }
        };
        if mode == PushMode::Full {
            assert!(manifests.is_empty().not(), "a full push sends a manifest");
        }

        let mut archives = BTreeMap::<u32, Vec<u8>>::new();
        let mut deleted = BTreeMap::new();

        loop {
            match self.recv().await {
                SyncFrame::Data { index, chunk } => {
                    archives.entry(index).or_default().extend(chunk);
                }
                SyncFrame::EntryDone {
                    index,
                    deleted: paths,
                } => {
                    if paths.is_empty().not() {
                        deleted.insert(
                            index,
                            paths
                                .into_iter()
                                .map(|path| String::from_utf8(path).unwrap())
                                .collect(),
                        );
                    }
                }
                SyncFrame::Commit { push_id: committed } => {
                    assert_eq!(committed, push_id);
                    break;
                }
                frame => panic!("unexpected frame in a push: {frame:?}"),
            }
        }

        let entries = plans
            .iter()
            .map(|plan| {
                let archive = archives.remove(&plan.index).unwrap_or_default();
                (plan.index, unpack(&archive))
            })
            .collect();

        Push {
            push_id,
            mode,
            entries,
            deleted,
            manifests,
        }
    }

    /// Answers push `push_id` as the sidecar would, with `generation`.
    async fn apply(&mut self, push_id: u64, generation: u64) {
        self.send(SyncFrame::Applied {
            push_id,
            generation,
            entries: Vec::new(),
        })
        .await;
    }

    /// Pairs the CLI, and receives and applies the full push that follows, as `generation`.
    async fn pair_and_apply(&mut self, generation: u64) -> Push {
        self.send(SyncFrame::Paired {
            generation: None,
            protocol_version: mirrord_operator::sync::SYNC_PROTOCOL_VERSION,
        })
        .await;
        let push = self.recv_push().await;
        assert_eq!(push.mode, PushMode::Full);
        self.apply(push.push_id, generation).await;
        self.held = push.manifests.clone();
        push
    }
}

/// The paths and contents of a gzip-compressed tar stream, `None` for a directory.
fn unpack(archive: &[u8]) -> Pushed {
    if archive.is_empty() {
        return Pushed::new();
    }

    tar::Archive::new(GzDecoder::new(archive))
        .entries()
        .unwrap()
        .map(|entry| {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let contents = entry.header().entry_type().is_file().then(|| {
                let mut contents = String::new();
                entry.read_to_string(&mut contents).unwrap();
                contents
            });
            (path, contents)
        })
        .collect()
}

/// A sync driver of `entries`, for a pod whose app a supervisor runs, started, whose every
/// connection reaches the test as an [`Operator`] on the returned channel.
fn start(
    entries: Vec<SyncEntry>,
    watch: bool,
) -> (
    JoinHandle<CliError>,
    mpsc::UnboundedReceiver<Operator>,
    tokio::sync::watch::Receiver<SyncActivity>,
) {
    start_with(entries, HandledBy::Supervisor, watch)
}

/// [`start`], for a pod that reports on a push as `handled_by` says.
fn start_with(
    entries: Vec<SyncEntry>,
    handled_by: HandledBy,
    watch: bool,
) -> (
    JoinHandle<CliError>,
    mpsc::UnboundedReceiver<Operator>,
    tokio::sync::watch::Receiver<SyncActivity>,
) {
    if watch {
        wait_for_earlier_fs_events();
    }

    let (operators, operators_rx) = mpsc::unbounded_channel();
    let connect: Connect<DuplexStream> = Box::new(move || {
        let operators = operators.clone();
        Box::pin(async move {
            let (cli, operator) = tokio::io::duplex(1 << 20);
            let (cli, operator) = tokio::join!(
                WebSocketStream::from_raw_socket(cli, Role::Client, None),
                WebSocketStream::from_raw_socket(operator, Role::Server, None),
            );
            let _ = operators.send(Operator {
                socket: operator,
                held: BTreeMap::new(),
            });
            Ok(PreviewSyncConnection::new(cli))
        })
    });

    let (driver, activity) = SyncDriver::with_connect(
        connect,
        entries.into(),
        handled_by,
        watch,
        ProgressTracker::null(),
    )
    .unwrap();
    (tokio::spawn(driver.run()), operators_rx, activity)
}

/// Returns once the file-system watcher reported every change that the test made so far, so that
/// the driver's watcher, which starts later, reports none of them.
///
/// FSEvents on macOS can report a change from shortly before a watch started as if it came
/// after. A test makes its tree right before the driver starts watching it, so the creation of
/// the entry's directory could reach the driver as an event. The driver would take the directory
/// for replaced, and make a full push where the test expects an incremental one or none.
///
/// FSEvents reports the changes of a volume in the order that they were made. So this makes
/// one more change, the marker file, and waits for a watcher of its own to report it.
fn wait_for_earlier_fs_events() {
    let marker_dir = tempfile::tempdir().unwrap();
    // The watcher's handler sends each event of `marker_dir` to this function.
    let (events_tx, events_rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let _ = events_tx.send(event);
    })
    .unwrap();
    watcher
        .watch(marker_dir.path(), RecursiveMode::NonRecursive)
        .unwrap();
    write(marker_dir.path(), "marker", "");

    loop {
        let event = events_rx
            .recv_timeout(FRAME_TIMEOUT)
            .expect("the watcher didn't report the marker file in time")
            .unwrap();
        // The watcher reports the path as it is on disk, which the temporary directory of macOS
        // reaches through a symlink, so only the name tells the marker.
        if event
            .paths
            .iter()
            .any(|path| path.file_name().is_some_and(|name| name == "marker"))
        {
            break;
        }
    }
}

/// The next connection that the driver opens.
async fn next_connection(operators: &mut mpsc::UnboundedReceiver<Operator>) -> Operator {
    tokio::time::timeout(FRAME_TIMEOUT, operators.recv())
        .await
        .expect("the CLI didn't connect in time")
        .unwrap()
}

fn write(dir: &Path, path: &str, contents: &str) {
    let path = dir.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn entry(local: &Path) -> SyncEntry {
    SyncEntry {
        local: local.to_owned(),
        remote: "/app".to_owned(),
        ignore: Vec::new(),
    }
}

fn file(contents: &str) -> Option<String> {
    Some(contents.to_owned())
}

/// After the first full push, `--watch` pushes each change incrementally: a new, a changed, a
/// deleted and a renamed file, and a new or renamed directory with everything in it, each in one
/// push that carries only what changed and lists what is gone. A change that the rules ignore
/// pushes nothing, and a change to the rules themselves makes a full push, because the
/// incremental pushes apply the rules of the last full push.
#[tokio::test]
async fn watch_pushes_each_change_incrementally() {
    let local = tempfile::tempdir().unwrap();
    let dir = local.path();
    write(dir, ".gitignore", "dist/\n");
    write(dir, "app.js", "v1");
    write(dir, "old.js", "old");
    write(dir, "lib/util.js", "util");

    let (sync, mut operators, _activity) = start(vec![entry(dir)], true);
    let mut operator = next_connection(&mut operators).await;
    let first = operator.pair_and_apply(1).await;
    assert_eq!(
        first.entries[&0],
        Pushed::from([
            (".gitignore".to_owned(), file("dist/\n")),
            ("app.js".to_owned(), file("v1")),
            ("lib".to_owned(), None),
            ("lib/util.js".to_owned(), file("util")),
            ("old.js".to_owned(), file("old")),
        ])
    );

    let mut generation = 1;
    let mut expect = async |operator: &mut Operator, pushed: Pushed, deleted: Vec<&str>| {
        let push = operator.recv_push().await;
        assert_eq!(
            push.mode,
            PushMode::Incremental {
                base_generation: generation
            }
        );
        assert_eq!(push.entries.get(&0).cloned().unwrap_or_default(), pushed);
        assert_eq!(
            push.deleted.get(&0).cloned().unwrap_or_default(),
            deleted
                .iter()
                .map(|path| path.to_string())
                .collect::<Vec<_>>()
        );
        generation += 1;
        operator.apply(push.push_id, generation).await;
    };

    write(dir, "app.js", "v2");
    write(dir, "new.js", "new");
    expect(
        &mut operator,
        Pushed::from([
            ("app.js".to_owned(), file("v2")),
            ("new.js".to_owned(), file("new")),
        ]),
        vec![],
    )
    .await;

    fs::remove_file(dir.join("old.js")).unwrap();
    expect(&mut operator, Pushed::new(), vec!["old.js"]).await;

    fs::rename(dir.join("new.js"), dir.join("renamed.js")).unwrap();
    expect(
        &mut operator,
        Pushed::from([("renamed.js".to_owned(), file("new"))]),
        vec!["new.js"],
    )
    .await;

    fs::rename(dir.join("lib"), dir.join("src")).unwrap();
    expect(
        &mut operator,
        Pushed::from([
            ("src".to_owned(), None),
            ("src/util.js".to_owned(), file("util")),
        ]),
        vec!["lib"],
    )
    .await;

    // A file in the renamed directory, which the CLI started watching when it pushed it.
    write(dir, "src/more.js", "more");
    expect(
        &mut operator,
        Pushed::from([("src/more.js".to_owned(), file("more"))]),
        vec![],
    )
    .await;

    write(dir, "dist/bundle.js", "bundle");
    assert_eq!(
        operator.recv_within(QUIET_PERIOD).await,
        None,
        "a change to an ignored path started a push"
    );

    write(dir, ".gitignore", "dist/\n*.log\n");
    let push = operator.recv_push().await;
    assert_eq!(push.mode, PushMode::Full);
    assert!(push.entries[&0].contains_key("src/more.js"));

    assert!(sync.is_finished().not());
    sync.abort();
}

/// The operator and the sidecar may still hold a push of a CLI that a newer CLI replaced, and
/// tell pushes apart by their ids, so the newer CLI's pushes must not reuse them.
#[tokio::test]
async fn two_clis_number_their_pushes_apart() {
    let local = tempfile::tempdir().unwrap();
    let dir = local.path();
    write(dir, "app.js", "v1");

    let mut push_ids = Vec::new();
    for _ in 0..2 {
        let (sync, mut operators, _activity) = start(vec![entry(dir)], true);
        let mut operator = next_connection(&mut operators).await;
        push_ids.push(operator.pair_and_apply(1).await.push_id);
        sync.abort();
    }

    assert_ne!(push_ids[0], push_ids[1]);
}

/// The operator unpairs the CLI when the sidecar goes, and the volume may then hold anything, so
/// the CLI makes a full push once it is paired again. A push the sidecar rejects because the
/// volume holds another generation than the one it built on is followed by a full push too.
#[tokio::test]
async fn a_full_push_follows_every_pairing_and_a_stale_base() {
    let local = tempfile::tempdir().unwrap();
    let dir = local.path();
    write(dir, "app.js", "v1");

    let (sync, mut operators, _activity) = start(vec![entry(dir)], true);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    operator
        .send(SyncFrame::Unpaired {
            reason: UnpairReason::SidecarGone,
        })
        .await;
    write(dir, "app.js", "v2");
    assert_eq!(
        operator.recv_within(QUIET_PERIOD).await,
        None,
        "the CLI pushed while unpaired"
    );
    let push = operator.pair_and_apply(2).await;
    assert_eq!(push.entries[&0]["app.js"], file("v2"));

    write(dir, "app.js", "v3");
    let push = operator.recv_push().await;
    assert_eq!(push.mode, PushMode::Incremental { base_generation: 2 });
    operator
        .send(SyncFrame::Rejected {
            push_id: push.push_id,
            reason: RejectReason::StaleBase,
        })
        .await;
    let push = operator.recv_push().await;
    assert_eq!(push.mode, PushMode::Full);
    assert_eq!(push.entries[&0]["app.js"], file("v3"));

    sync.abort();
}

/// A full push sends the manifest of the whole tree and carries only the paths that the sidecar
/// wants, so a push after a pairing sends what changed and keeps the rest, which the manifest
/// still lists.
#[tokio::test]
async fn a_full_push_carries_only_what_the_sidecar_wants() {
    let local = tempfile::tempdir().unwrap();
    let dir = local.path();
    write(dir, "app.js", "v1");
    write(dir, "lib/util.js", "util");

    let (sync, mut operators, _activity) = start(vec![entry(dir)], false);
    let mut operator = next_connection(&mut operators).await;
    let push = operator.pair_and_apply(1).await;
    assert_eq!(
        push.entries[&0],
        Pushed::from([
            ("app.js".to_owned(), file("v1")),
            ("lib".to_owned(), None),
            ("lib/util.js".to_owned(), file("util")),
        ])
    );

    write(dir, "app.js", "v2");
    let push = operator.pair_and_apply(2).await;
    assert_eq!(
        push.entries[&0],
        Pushed::from([("app.js".to_owned(), file("v2"))])
    );
    let listed = push.manifests[&0]
        .iter()
        .map(|path| String::from_utf8(path.path.clone()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(listed, ["app.js", "lib", "lib/util.js"]);

    sync.abort();
}

/// A full push that started from changes lists the trees, exchanges their manifests, and only then
/// reads the files that the sidecar wants. A tool can remove a file in between, and under
/// `--watch` that must not end the sync: the next change, such as the removal, starts another
/// full push on the same connection.
#[tokio::test]
async fn a_file_deleted_before_its_full_push_packs_it_doesnt_end_watch() {
    let local = tempfile::tempdir().unwrap();
    let dir = local.path();
    write(dir, "app.js", "v1");

    let (sync, mut operators, _activity) = start(vec![entry(dir)], true);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    // A rule change starts a full push, and `app.js` goes after the CLI hashed it.
    write(dir, ".gitignore", "*.log\n");
    loop {
        match operator.recv().await {
            SyncFrame::Manifest { .. } => {}
            SyncFrame::ManifestDone { push_id } => {
                fs::remove_file(dir.join("app.js")).unwrap();
                operator
                    .send(SyncFrame::Want {
                        push_id,
                        index: 0,
                        paths: vec![b"app.js".to_vec()],
                    })
                    .await;
                operator.send(SyncFrame::WantDone { push_id }).await;
                break;
            }
            frame => panic!("expected the manifest of a full push, got {frame:?}"),
        }
    }
    // The removal is itself a change, which starts the next full push on the same connection,
    // where a push without the wanted file would have been rejected.
    let push = operator.recv_push().await;
    assert_eq!(push.mode, PushMode::Full);
    assert!(
        push.manifests[&0]
            .iter()
            .all(|path| path.path != b"app.js".to_vec())
    );

    assert!(sync.is_finished().not());
    sync.abort();
}

/// A change while the preview is idle wakes it once, and the CLI makes a full push once the
/// operator pairs it again.
#[tokio::test]
async fn a_change_wakes_an_idle_preview() {
    let local = tempfile::tempdir().unwrap();
    let dir = local.path();
    write(dir, "app.js", "v1");

    let (sync, mut operators, _activity) = start(vec![entry(dir)], true);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    operator
        .send(SyncFrame::Unpaired {
            reason: UnpairReason::Idle,
        })
        .await;
    write(dir, "app.js", "v2");
    assert_eq!(operator.recv().await, SyncFrame::WakeRequest);

    write(dir, "app.js", "v3");
    assert_eq!(
        operator.recv_within(QUIET_PERIOD).await,
        None,
        "the CLI woke the preview twice, or pushed while unpaired"
    );

    let push = operator.pair_and_apply(2).await;
    assert_eq!(push.entries[&0]["app.js"], file("v3"));

    sync.abort();
}

/// A push that fails with an I/O error of the sidecar is retried once on a new connection, and
/// fails the sync when the retry fails too. A rejection that would repeat, such as a conflict,
/// fails the sync without a retry.
#[tokio::test]
async fn a_failed_push_is_retried_once_unless_it_would_repeat() {
    let local = tempfile::tempdir().unwrap();
    write(local.path(), "app.js", "v1");
    let io = || RejectReason::Io("disk error".to_owned());

    let (sync, mut operators, _activity) = start(vec![entry(local.path())], false);
    for attempt in 0..2 {
        let mut operator = next_connection(&mut operators).await;
        operator
            .send(SyncFrame::Paired {
                generation: None,
                protocol_version: mirrord_operator::sync::SYNC_PROTOCOL_VERSION,
            })
            .await;
        let push = operator.recv_push().await;
        operator
            .send(SyncFrame::Rejected {
                push_id: push.push_id,
                reason: io(),
            })
            .await;
        if attempt == 1 {
            let error = tokio::time::timeout(FRAME_TIMEOUT, sync)
                .await
                .expect("the sync didn't end after the failed retry")
                .unwrap();
            assert!(
                super::with_causes(&error).contains("disk error"),
                "{error:?}"
            );
            break;
        }
    }

    let (sync, mut operators, _activity) = start(vec![entry(local.path())], false);
    let mut operator = next_connection(&mut operators).await;
    operator
        .send(SyncFrame::Paired {
            generation: None,
            protocol_version: mirrord_operator::sync::SYNC_PROTOCOL_VERSION,
        })
        .await;
    let push = operator.recv_push().await;
    operator
        .send(SyncFrame::Rejected {
            push_id: push.push_id,
            reason: RejectReason::Conflict("node_modules".to_owned()),
        })
        .await;
    let error = tokio::time::timeout(FRAME_TIMEOUT, sync)
        .await
        .expect("the sync didn't end after the push failed")
        .unwrap();
    assert!(
        super::with_causes(&error).contains("node_modules"),
        "{error:?}"
    );
    assert!(
        operators.try_recv().is_err(),
        "the CLI connected again after a rejection that would repeat"
    );
}

/// When another CLI takes over the sync, this one stops and doesn't connect again.
#[tokio::test]
async fn a_replaced_sync_stops_without_reconnecting() {
    let local = tempfile::tempdir().unwrap();
    write(local.path(), "app.js", "v1");

    let (sync, mut operators, _activity) = start(vec![entry(local.path())], true);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;
    operator
        .send(SyncFrame::Unpaired {
            reason: UnpairReason::Replaced,
        })
        .await;

    let result = tokio::time::timeout(FRAME_TIMEOUT, sync)
        .await
        .expect("the sync didn't end after another CLI took over")
        .unwrap();
    assert!(
        matches!(result, CliError::PreviewSyncReplaced),
        "{result:?}"
    );
    assert!(operators.try_recv().is_err());
}

/// `mirrord preview sync` runs no creation timeout while the preview pod is busy with work whose
/// length the CLI can't predict, such as copying the image's files or `prepare`, so the
/// activity says when the pod is busy, and counts every report of progress. The run succeeds only
/// once the supervisor is done with the first push, so the activity says which push it handled.
#[tokio::test]
async fn the_activity_reports_busy_work_and_counts_progress() {
    let local = tempfile::tempdir().unwrap();
    write(local.path(), "app.js", "v1");

    let (sync, mut operators, mut activity) = start(vec![entry(local.path())], false);
    let mut operator = next_connection(&mut operators).await;
    let next = async |activity: &mut tokio::sync::watch::Receiver<SyncActivity>| {
        tokio::time::timeout(FRAME_TIMEOUT, activity.changed())
            .await
            .expect("the activity didn't change")
            .unwrap();
        *activity.borrow_and_update()
    };

    operator
        .send(SyncFrame::Status(SidecarState::Seeding))
        .await;
    let seeding = next(&mut activity).await;
    assert!(seeding.busy);

    operator
        .send(SyncFrame::Status(SidecarState::AwaitingFirstPush))
        .await;
    let awaiting = next(&mut activity).await;
    assert!(awaiting.busy.not());
    assert!(awaiting.progress > seeding.progress);

    operator.pair_and_apply(1).await;
    let pushed = loop {
        let current = next(&mut activity).await;
        if current.first_push.is_some() {
            break current;
        }
    };
    assert_eq!(pushed.first_push, Some(1));
    assert!(pushed.busy.not());

    let preparing = SupervisorReport {
        prepare: PrepareReport {
            state: PrepareState::Running,
            error: None,
        },
        app: AppReport::default(),
        handled: None,
    };
    operator.send(SyncFrame::Supervisor(preparing)).await;
    assert!(next(&mut activity).await.busy);

    let running = SupervisorReport {
        prepare: PrepareReport {
            state: PrepareState::Succeeded,
            error: None,
        },
        app: AppReport {
            state: AppState::Running,
            restarts: 0,
            last_exit_code: None,
        },
        handled: Some(1),
    };
    operator.send(SyncFrame::Supervisor(running.clone())).await;
    let prepared = next(&mut activity).await;
    assert!(prepared.busy.not());
    assert_eq!(prepared.handled, Some(1));
    assert!(prepared.prepare_failed.not());

    sync.abort();
}

/// Without `run`, the preview pod runs its container's own command, and no supervisor reports
/// which push it handled. `mirrord preview sync` returns only once a push counts as handled, so
/// the push counts as handled as soon as the volume holds it.
#[tokio::test]
async fn a_push_into_a_pod_without_a_supervisor_counts_as_handled_once_applied() {
    let local = tempfile::tempdir().unwrap();
    write(local.path(), "index.html", "v1");

    let (sync, mut operators, mut activity) =
        start_with(vec![entry(local.path())], HandledBy::Volume, false);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    let applied = *tokio::time::timeout(
        FRAME_TIMEOUT,
        activity.wait_for(|activity| activity.first_push.is_some()),
    )
    .await
    .expect("the push wasn't applied in time")
    .unwrap();
    assert_eq!(applied.first_push, Some(1));
    assert_eq!(applied.handled, Some(1));

    sync.abort();
}

/// The supervisor retries an app that fails to start for as long as it fails, and each retry
/// counts up the app's restarts in its report. Those retries aren't progress, or an app that never
/// starts would keep `mirrord preview sync` from ever timing out.
#[tokio::test]
async fn an_app_that_keeps_failing_to_start_is_no_progress() {
    let local = tempfile::tempdir().unwrap();
    write(local.path(), "app.js", "v1");

    let (sync, mut operators, mut activity) = start(vec![entry(local.path())], false);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;
    let pushed = tokio::time::timeout(
        FRAME_TIMEOUT,
        activity.wait_for(|activity| activity.first_push.is_some()),
    )
    .await
    .expect("the first push didn't land")
    .unwrap()
    .progress;

    let report = |state, restarts, handled| SupervisorReport {
        prepare: PrepareReport::default(),
        app: AppReport {
            state,
            restarts,
            last_exit_code: Some(127),
        },
        handled,
    };
    for restarts in 1..=3 {
        operator
            .send(SyncFrame::Supervisor(report(
                AppState::Restarting,
                restarts,
                None,
            )))
            .await;
    }
    // The driver reads the frames in order, so once it saw this one, it saw the retries too.
    operator
        .send(SyncFrame::Supervisor(report(AppState::Running, 3, Some(1))))
        .await;

    let handled = tokio::time::timeout(
        FRAME_TIMEOUT,
        activity.wait_for(|activity| activity.handled == Some(1)),
    )
    .await
    .expect("the supervisor didn't handle the first push")
    .unwrap()
    .progress;

    // The app's first `Restarting` and its `Running` are progress, the two retries aren't.
    assert_eq!(handled - pushed, 2);
    sync.abort();
}

/// The operator unpairs the CLI in the middle of a push when the pod's sidecar goes away or a
/// `mirrord exec` session pauses the preview. The CLI must then wait for the next `Paired` and
/// make a full push, rather than fail the sync.
#[tokio::test]
async fn an_unpair_during_a_push_waits_for_the_next_pairing() {
    let local = tempfile::tempdir().unwrap();
    let dir = local.path();
    write(dir, "app.js", "v1");

    let (sync, mut operators, _activity) = start(vec![entry(dir)], true);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    write(dir, "app.js", "v2");
    let push = operator.recv_push().await;
    assert_eq!(push.mode, PushMode::Incremental { base_generation: 1 });
    operator
        .send(SyncFrame::Unpaired {
            reason: UnpairReason::SidecarGone,
        })
        .await;

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(sync.is_finished().not(), "{:?}", sync.await);

    let push = operator.pair_and_apply(2).await;
    assert_eq!(push.entries[&0]["app.js"], file("v2"));
    sync.abort();
}

/// `mirrord preview sync` waits until the supervisor is done with the first push. A pod whose
/// `emptyDir` volume is replaced starts its generations again, so after a new pairing the CLI
/// must wait for the first push of that pairing, not for a generation that the new volume may
/// never reach.
#[tokio::test]
async fn each_pairing_has_its_own_first_push() {
    let local = tempfile::tempdir().unwrap();
    write(local.path(), "app.js", "v1");

    let (sync, mut operators, mut activity) = start(vec![entry(local.path())], false);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(3).await;
    tokio::time::timeout(
        FRAME_TIMEOUT,
        activity.wait_for(|activity| activity.first_push == Some(3)),
    )
    .await
    .expect("the first push of the new pairing didn't land")
    .unwrap();

    operator
        .send(SyncFrame::Unpaired {
            reason: UnpairReason::SidecarGone,
        })
        .await;
    operator.pair_and_apply(1).await;
    tokio::time::timeout(
        FRAME_TIMEOUT,
        activity.wait_for(|activity| activity.first_push == Some(1)),
    )
    .await
    .expect("the first push of the new pairing is the one to wait for")
    .unwrap();
    sync.abort();
}

/// Under `--watch`, the run lasts as long as the developer works, and every operator restart
/// breaks the connection between pushes. The CLI must connect again each time.
#[tokio::test]
async fn watch_reconnects_after_every_broken_connection() {
    let local = tempfile::tempdir().unwrap();
    write(local.path(), "app.js", "v1");

    let (sync, mut operators, _activity) = start(vec![entry(local.path())], true);
    for generation in 1..=3 {
        let mut operator = next_connection(&mut operators).await;
        operator.pair_and_apply(generation).await;
        drop(operator);
    }

    next_connection(&mut operators).await;
    assert!(sync.is_finished().not(), "{:?}", sync.await);
    sync.abort();
}

/// The creation timeout doesn't run while `prepare` runs. Once the operator unpairs the CLI
/// because the sidecar is gone, the pod that ran `prepare` may be gone too, so the CLI must
/// stop reporting the pod as busy, or the timeout would never run again if no pod comes back.
#[tokio::test]
async fn an_unpair_ends_the_busy_prepare() {
    let local = tempfile::tempdir().unwrap();
    write(local.path(), "app.js", "v1");

    let (sync, mut operators, mut activity) = start(vec![entry(local.path())], false);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    operator
        .send(SyncFrame::Supervisor(SupervisorReport {
            prepare: PrepareReport {
                state: PrepareState::Running,
                error: None,
            },
            app: AppReport::default(),
            handled: None,
        }))
        .await;
    tokio::time::timeout(FRAME_TIMEOUT, activity.wait_for(|activity| activity.busy))
        .await
        .expect("the CLI didn't report the pod busy")
        .unwrap();

    operator
        .send(SyncFrame::Unpaired {
            reason: UnpairReason::SidecarGone,
        })
        .await;
    tokio::time::timeout(
        FRAME_TIMEOUT,
        activity.wait_for(|activity| activity.busy.not()),
    )
    .await
    .expect("the CLI reports the pod busy after the sidecar is gone")
    .unwrap();
    sync.abort();
}

/// A broken connection delivers no `Unpaired`, and the CLI may not reach the operator again for
/// a while, so the `prepare` that the connection reported running must stop keeping the creation
/// timeout stopped too.
#[tokio::test]
async fn a_broken_connection_ends_the_busy_prepare() {
    let local = tempfile::tempdir().unwrap();
    write(local.path(), "app.js", "v1");

    let (sync, mut operators, mut activity) = start(vec![entry(local.path())], true);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    operator
        .send(SyncFrame::Supervisor(SupervisorReport {
            prepare: PrepareReport {
                state: PrepareState::Running,
                error: None,
            },
            app: AppReport::default(),
            handled: None,
        }))
        .await;
    tokio::time::timeout(FRAME_TIMEOUT, activity.wait_for(|activity| activity.busy))
        .await
        .expect("the CLI didn't report the pod busy")
        .unwrap();

    drop(operator);
    tokio::time::timeout(
        FRAME_TIMEOUT,
        activity.wait_for(|activity| activity.busy.not()),
    )
    .await
    .expect("the CLI reports the pod busy after the connection broke")
    .unwrap();
    sync.abort();
}

/// A file that the CLI can't read fails the pack of its batch, and the other changes of the batch
/// must still reach the pod, or the pod would run old code until each of them changes again.
#[tokio::test]
async fn changes_that_fail_to_pack_are_pushed_with_the_next_change() {
    let local = tempfile::tempdir().unwrap();
    let dir = local.path();
    write(dir, "a.js", "v1");
    write(dir, "b.js", "v1");

    let (sync, mut operators, _activity) = start(vec![entry(dir)], true);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    write(dir, "a.js", "v2");
    write(dir, "b.js", "v2");
    fs::set_permissions(dir.join("b.js"), fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads the file anyway, and the batch then packs.
    let first = operator.recv_within(QUIET_PERIOD).await;
    fs::set_permissions(dir.join("b.js"), fs::Permissions::from_mode(0o644)).unwrap();

    let push = match first {
        Some(SyncFrame::Begin { .. }) => {
            sync.abort();
            return;
        }
        Some(frame) => panic!("unexpected frame {frame:?}"),
        None => operator.recv_push().await,
    };
    assert_eq!(push.entries[&0]["a.js"], file("v2"));
    assert_eq!(push.entries[&0]["b.js"], file("v2"));
    sync.abort();
}

/// A tool can replace the whole local directory, for example by renaming a fresh copy into its
/// place, and the watches stay on the directory that it renamed away. `--watch` must still push
/// the directory that is there now, or the preview keeps the old code for the rest of the run.
#[tokio::test]
async fn watch_pushes_a_directory_that_replaces_the_local_one() {
    let parent = tempfile::tempdir().unwrap();
    let local = parent.path().join("app");
    write(&local, "main.js", "old code");
    let replacement = parent.path().join("replacement");
    write(&replacement, "main.js", "new code");

    let (sync, mut operators, _activity) = start(vec![entry(&local)], true);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    fs::rename(&local, parent.path().join("old-app")).unwrap();
    fs::rename(&replacement, &local).unwrap();
    let push = operator.recv_push().await;
    assert_eq!(push.mode, PushMode::Full);
    assert_eq!(push.entries[&0]["main.js"], file("new code"));
    sync.abort();
}

/// A `.gitignore` that the developer creates in a directory above the pushed one changes the
/// rules, so `--watch` must apply it to what it pushes next, even though the file didn't exist
/// when the CLI started watching.
#[tokio::test]
async fn a_gitignore_created_above_the_tree_changes_what_watch_pushes() {
    let repository = tempfile::tempdir().unwrap();
    fs::create_dir(repository.path().join(".git")).unwrap();
    let service = repository.path().join("service");
    write(&service, "app.js", "v1");

    let (sync, mut operators, _activity) = start(vec![entry(&service)], true);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    write(repository.path(), ".gitignore", ".env\n");
    let push = operator.recv_push().await;
    assert_eq!(push.mode, PushMode::Full);
    operator.apply(push.push_id, 2).await;

    write(&service, ".env", "secret");
    assert_eq!(operator.recv_within(QUIET_PERIOD).await, None);
    sync.abort();
}

/// Git reads a `.gitignore` even when a rule, here the entry's `ignore` list, matches the file
/// itself. A rule that the developer adds to it must keep the file it names out of the next
/// pushes, or `--watch` would send a secret that the developer just ignored.
#[tokio::test]
async fn an_ignored_gitignore_still_keeps_what_it_names_out_of_watch() {
    let local = tempfile::tempdir().unwrap();
    let dir = local.path();
    write(dir, "app.js", "v1");
    write(dir, ".gitignore", "*.log\n");
    let entry = SyncEntry {
        ignore: vec![".gitignore".to_owned()],
        ..entry(dir)
    };

    let (sync, mut operators, _activity) = start(vec![entry], true);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    write(dir, ".gitignore", "*.log\n.env\n");
    let push = operator.recv_push().await;
    assert_eq!(push.mode, PushMode::Full);
    operator.apply(push.push_id, 2).await;

    write(dir, ".env", "secret");
    assert_eq!(operator.recv_within(QUIET_PERIOD).await, None);
    sync.abort();
}

/// Two `sync` entries may have local directories inside one another, and a file under both is in
/// both remotes, so a change to it must reach both.
#[tokio::test]
async fn a_change_under_two_local_directories_reaches_both_remotes() {
    let local = tempfile::tempdir().unwrap();
    let dir = local.path();
    write(dir, "nested/app.js", "v1");
    let nested = SyncEntry {
        local: dir.join("nested"),
        remote: "/nested".to_owned(),
        ignore: Vec::new(),
    };

    let (sync, mut operators, _activity) = start(vec![entry(dir), nested], true);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    write(dir, "nested/app.js", "v2");
    let push = operator.recv_push().await;
    assert_eq!(push.entries[&0]["nested/app.js"], file("v2"));
    assert_eq!(push.entries[&1]["app.js"], file("v2"));
    sync.abort();
}

/// A service of a `mirrord-up.yaml` in `/repo` with the given `sync` entries, as
/// `(local, remote)`, and `prepare`. The preview runs `npm run dev` in `/app`.
fn service(entries: &[(&str, &str)], prepare: Option<PrepareConfig>) -> SubprocessCfg {
    SubprocessCfg {
        config: LayerConfig::resolve(&mut ConfigContext::default()).unwrap(),
        service_name: Arc::from("frontend"),
        run: RunConfig {
            r#type: RunType::Exec,
            directory: None,
            command: vec!["npm".to_owned(), "start".to_owned()],
            pre_command: Vec::new(),
        },
        sync: Some(SyncConfig {
            files: entries
                .iter()
                .map(|(local, remote)| SyncEntry {
                    local: Path::new("/repo").join(local),
                    remote: (*remote).to_owned(),
                    ignore: Vec::new(),
                })
                .collect(),
            prepare,
            run: Some(SyncRunConfig {
                command: vec!["npm".to_owned(), "run".to_owned(), "dev".to_owned()],
                directory: Some("/app".to_owned()),
                restart: SyncRestart::OnSync,
            }),
        }),
        mode: ServiceMode::default().into(),
        incoming: ServiceMode::default(),
        db_branches_origin: RequestOrigin::Config,
        queues_origin: RequestOrigin::Config,
    }
}

/// `prepare.triggered_by` globs are local paths, which `mirrord preview sync` resolved against
/// the directory of `mirrord-up.yaml`, but the preview pod matches them against its own files, so
/// each must become the same glob under the `remote` that holds the files. Without `sync.run`,
/// the preview keeps its image's command rather than taking `run.command`, which runs the service
/// on the developer's machine.
#[test]
fn triggered_by_globs_match_the_synced_files_in_the_container() {
    let prepare = PrepareConfig {
        command: vec!["npm".to_owned(), "ci".to_owned()],
        directory: Some("/app/packages".to_owned()),
        triggered_by: vec![
            "/repo/frontend/package-lock.json".into(),
            "/repo/backend/../frontend/./packages/ui/**/*.json".into(),
            "/repo/frontend/**/requirements*.txt".into(),
        ],
        persist: vec!["/usr/local/lib/node".to_owned()],
    };
    let frontend = service(
        &[("frontend", "/app"), ("frontend/packages/ui", "/ui")],
        Some(prepare),
    );

    let sync = ServiceSync::of_service(&frontend).unwrap().unwrap();

    assert_eq!(sync.entries, frontend.sync.as_ref().unwrap().files);
    assert_eq!(
        sync.config,
        PreviewSyncConfig {
            entries: vec![
                PreviewSyncEntry {
                    remote: "/app".to_owned()
                },
                PreviewSyncEntry {
                    remote: "/ui".to_owned()
                },
            ],
            run: Some(PreviewSyncRun {
                command: vec!["npm".to_owned(), "run".to_owned(), "dev".to_owned()],
                working_dir: Some("/app".to_owned()),
                restart: PreviewSyncRestart::OnSync,
            }),
            prepare: Some(PreviewSyncPrepare {
                command: vec!["npm".to_owned(), "ci".to_owned()],
                triggered_by: vec![
                    "/app/package-lock.json".to_owned(),
                    "/ui/**/*.json".to_owned(),
                    "/app/**/requirements*.txt".to_owned(),
                ],
                persist: vec!["/usr/local/lib/node".to_owned()],
                working_dir: Some("/app/packages".to_owned()),
            }),
        }
    );

    let mut image_command = service(&[("frontend", "/app")], None);
    image_command.sync.as_mut().unwrap().run = None;
    let sync = ServiceSync::of_service(&image_command).unwrap().unwrap();
    assert_eq!(sync.config.run, None);
}

/// The preview pod only has the synced files, runs `prepare` only under the supervisor that `run`
/// starts, and the CLI syncs into one running pod, so a glob outside every `local`, a `prepare`
/// without `run`, settings that the pod can't mount, an idle start, more than one replica and a
/// CronJob target must fail before anything changes in the cluster.
#[test]
fn settings_that_a_sync_preview_cannot_run_are_refused() {
    let with_glob = |glob: &str| {
        service(
            &[("frontend", "/app")],
            Some(PrepareConfig {
                command: vec!["npm".to_owned(), "ci".to_owned()],
                directory: None,
                triggered_by: vec![glob.into()],
                persist: Vec::new(),
            }),
        )
    };

    for glob in [
        "/repo/backend/requirements.txt",
        "/repo/frontend",
        "/repo/frontend-old/package.json",
    ] {
        assert!(
            matches!(
                ServiceSync::of_service(&with_glob(glob)),
                Err(CliError::PreviewSyncTriggeredByOutsideLocal(refused)) if refused == Path::new(glob)
            ),
            "{glob}"
        );
    }

    let mut prepare_without_run = with_glob("/repo/frontend/package-lock.json");
    prepare_without_run.sync.as_mut().unwrap().run = None;
    assert!(matches!(
        ServiceSync::of_service(&prepare_without_run),
        Err(CliError::PreviewSyncConfigInvalid(
            PreviewSyncConfigError::PrepareWithoutRun
        ))
    ));

    let mut start_idle = service(&[("frontend", "/app")], None);
    start_idle.config.feature.preview.idle.start_idle = true;
    assert!(matches!(
        ServiceSync::of_service(&start_idle),
        Err(CliError::PreviewSyncStartIdle)
    ));

    let mut replicas = service(&[("frontend", "/app")], None);
    replicas.config.feature.preview.replicas = 2;
    assert!(matches!(
        ServiceSync::of_service(&replicas),
        Err(CliError::PreviewSyncReplicas(2))
    ));

    let mut cron_job = service(&[("frontend", "/app")], None);
    cron_job.config.target.path = Some(Target::CronJob(CronJobTarget {
        cron_job: "report".to_owned(),
        container: None,
    }));
    assert!(matches!(
        ServiceSync::of_service(&cron_job),
        Err(CliError::PreviewSyncCronJobTarget)
    ));

    assert!(matches!(
        ServiceSync::of_service(&service(&[("frontend", "/mirrord/app")], None)),
        Err(CliError::PreviewSyncConfigInvalid(PreviewSyncConfigError::Reserved { path, .. }))
            if path == "/mirrord/app"
    ));
}

/// A full push that can't pack the files that the sidecar wants ends without a push, so the
/// preview pod isn't busy with it any more, and the creation timeout must run again until the next
/// change starts the next push.
#[tokio::test]
async fn a_full_push_that_fails_to_pack_after_the_exchange_frees_the_pod() {
    let local = tempfile::tempdir().unwrap();
    let dir = local.path();
    write(dir, "app.js", "v1");

    let (sync, mut operators, mut activity) = start(vec![entry(dir)], true);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;
    tokio::time::timeout(
        FRAME_TIMEOUT,
        activity.wait_for(|activity| activity.busy.not()),
    )
    .await
    .expect("the CLI didn't report the pod free")
    .unwrap();

    // A rule change starts a full push, and `app.js` goes after the CLI hashed it.
    write(dir, ".gitignore", "*.log\n");
    loop {
        match operator.recv().await {
            SyncFrame::Manifest { .. } => {}
            SyncFrame::ManifestDone { push_id } => {
                fs::remove_file(dir.join("app.js")).unwrap();
                operator
                    .send(SyncFrame::Want {
                        push_id,
                        index: 0,
                        paths: vec![b"app.js".to_vec()],
                    })
                    .await;
                operator.send(SyncFrame::WantDone { push_id }).await;
                break;
            }
            frame => panic!("expected the manifest of a full push, got {frame:?}"),
        }
    }

    // The next push starts only once the removal's batch is quiet for `WATCH_DEBOUNCE`.
    let free = tokio::time::timeout(
        Duration::from_millis(150),
        activity.wait_for(|activity| activity.busy.not()),
    )
    .await;
    sync.abort();
    assert!(
        free.is_ok(),
        "the CLI reports the pod busy after the push that it couldn't pack ended"
    );
}

/// A connection that the operator or the API server refuses with a 4xx, such as for a missing
/// certificate, a refused license or a role without `previews/sync`, fails the sync with the
/// reason at once. Retrying it would only print warnings, and during the start it would end in a
/// timeout that hides the reason. A request timeout, a rate limit and a server error may pass.
#[test]
fn a_refused_connection_isnt_retried() {
    let stop = |code| {
        let mut status = kube::core::Status::failure("refused", "Refused");
        status.code = code;
        Stop::from(OperatorApiError::KubeError {
            error: kube::Error::Api(Box::new(status)),
            operation: OperatorOperation::WebsocketConnection,
        })
    };

    for code in [400, 401, 403, 404, 409] {
        assert!(matches!(stop(code), Stop::Fatal(_)), "{code}");
    }
    for code in [408, 429, 500, 503] {
        assert!(matches!(stop(code), Stop::Disconnected(_)), "{code}");
    }
}
