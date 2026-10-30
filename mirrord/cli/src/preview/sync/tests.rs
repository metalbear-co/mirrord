#[cfg(unix)]
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
    client::preview_sync::PreviewSyncConnection,
    crd::preview::{
        PreviewSyncConfig, PreviewSyncConfigError, PreviewSyncEntry, PreviewSyncInstall,
    },
    sync::{
        AppReport, AppState, InstallReport, InstallState, PushMode, RejectReason, SidecarState,
        SupervisorReport, SyncFrame, UnpairReason,
    },
};
use mirrord_progress::ProgressTracker;
use mirrord_up::{InstallConfig, RunConfig, RunType, ServiceMode, SubprocessCfg, SyncEntry};
use tokio::{io::DuplexStream, sync::mpsc, task::JoinHandle};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

use super::{Connect, ServiceSync, SyncActivity, SyncDriver, WATCH_DEBOUNCE, WATCH_MAX_DELAY};
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
}

/// The operator's end of one sync connection, which a test scripts.
struct Operator {
    socket: WebSocketStream<DuplexStream>,
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

    /// Receives a whole push, from `Begin` to `Commit`, and unpacks it.
    async fn recv_push(&mut self) -> Push {
        let SyncFrame::Begin {
            push_id,
            mode,
            entries: plans,
        } = self.recv().await
        else {
            panic!("a push starts with `Begin`");
        };

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

/// A sync driver of `entries`, started, whose every connection reaches the test as an
/// [`Operator`] on the returned channel.
fn start(
    entries: Vec<SyncEntry>,
    watch: bool,
) -> (
    JoinHandle<CliError>,
    mpsc::UnboundedReceiver<Operator>,
    tokio::sync::watch::Receiver<SyncActivity>,
) {
    let (operators, operators_rx) = mpsc::unbounded_channel();
    let connect: Connect<DuplexStream> = Box::new(move || {
        let operators = operators.clone();
        Box::pin(async move {
            let (cli, operator) = tokio::io::duplex(1 << 20);
            let (cli, operator) = tokio::join!(
                WebSocketStream::from_raw_socket(cli, Role::Client, None),
                WebSocketStream::from_raw_socket(operator, Role::Server, None),
            );
            let _ = operators.send(Operator { socket: operator });
            Ok(PreviewSyncConnection::new(cli))
        })
    });

    let (driver, activity) =
        SyncDriver::with_connect(connect, entries.into(), watch, ProgressTracker::null()).unwrap();
    (tokio::spawn(driver.run()), operators_rx, activity)
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
/// pushes nothing, and a change to the rules themselves pushes the tree whole, because the
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
/// the CLI pushes the tree whole once it is paired again. A push the sidecar rejects because the
/// volume holds another generation than the one it built on is pushed whole too.
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

/// A change while the preview is idle wakes it once, and the CLI pushes the tree whole once the
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
                .unwrap()
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
        .unwrap()
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
        .unwrap()
        .unwrap();
    assert!(
        matches!(result, CliError::PreviewSyncReplaced),
        "{result:?}"
    );
    assert!(operators.try_recv().is_err());
}

/// `mirrord preview sync` runs no creation timeout while the preview pod is busy with work whose
/// length the CLI can't predict, such as copying the image's files or an install, so the
/// activity says when the pod is busy, and counts every report of progress.
#[tokio::test]
async fn the_activity_reports_busy_work_and_counts_progress() {
    let local = tempfile::tempdir().unwrap();
    write(local.path(), "app.js", "v1");

    let (sync, mut operators, mut activity) = start(vec![entry(local.path())], false);
    let mut operator = next_connection(&mut operators).await;
    let next = async |activity: &mut tokio::sync::watch::Receiver<SyncActivity>| {
        tokio::time::timeout(FRAME_TIMEOUT, activity.changed())
            .await
            .unwrap()
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

    let installing = SupervisorReport {
        install: InstallReport {
            state: InstallState::Running,
            error: None,
        },
        app: AppReport::default(),
        handled: None,
    };
    operator.send(SyncFrame::Supervisor(installing)).await;
    assert!(next(&mut activity).await.busy);

    let running = SupervisorReport {
        install: InstallReport {
            state: InstallState::Succeeded,
            error: None,
        },
        app: AppReport {
            state: AppState::Running,
            restarts: 0,
            last_exit_code: None,
        },
        handled: Some(1),
    };
    operator.send(SyncFrame::Supervisor(running)).await;
    assert!(next(&mut activity).await.busy.not());

    sync.abort();
}

/// The operator unpairs the CLI in the middle of a push when the pod's sidecar goes away or a
/// `mirrord exec` session pauses the preview. The CLI must then wait for the next `Paired` and
/// push the tree whole, rather than fail the sync.
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

/// The creation timeout doesn't run while the install runs. Once the operator unpairs the CLI
/// because the sidecar is gone, the pod that ran the install may be gone too, so the CLI must
/// stop reporting the pod as busy, or the timeout would never run again if no pod comes back.
#[tokio::test]
async fn an_unpair_ends_the_busy_install() {
    let local = tempfile::tempdir().unwrap();
    write(local.path(), "app.js", "v1");

    let (sync, mut operators, mut activity) = start(vec![entry(local.path())], false);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    operator
        .send(SyncFrame::Supervisor(SupervisorReport {
            install: InstallReport {
                state: InstallState::Running,
                error: None,
            },
            app: AppReport::default(),
            handled: None,
        }))
        .await;
    tokio::time::timeout(FRAME_TIMEOUT, activity.wait_for(|activity| activity.busy))
        .await
        .unwrap()
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
/// a while, so the install that the connection reported running must stop keeping the creation
/// timeout stopped too.
#[tokio::test]
async fn a_broken_connection_ends_the_busy_install() {
    let local = tempfile::tempdir().unwrap();
    write(local.path(), "app.js", "v1");

    let (sync, mut operators, mut activity) = start(vec![entry(local.path())], true);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    operator
        .send(SyncFrame::Supervisor(SupervisorReport {
            install: InstallReport {
                state: InstallState::Running,
                error: None,
            },
            app: AppReport::default(),
            handled: None,
        }))
        .await;
    tokio::time::timeout(FRAME_TIMEOUT, activity.wait_for(|activity| activity.busy))
        .await
        .unwrap()
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
#[cfg(unix)]
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
/// `(local, remote)`, and `install`.
fn service(entries: &[(&str, &str)], install: Option<InstallConfig>) -> SubprocessCfg {
    SubprocessCfg {
        config: LayerConfig::resolve(&mut ConfigContext::default()).unwrap(),
        service_name: Arc::from("frontend"),
        run: RunConfig {
            r#type: RunType::Exec,
            directory: None,
            command: vec!["npm".to_owned(), "run".to_owned(), "dev".to_owned()],
            pre_command: Vec::new(),
            remote_directory: Some("/app".to_owned()),
        },
        sync: entries
            .iter()
            .map(|(local, remote)| SyncEntry {
                local: Path::new("/repo").join(local),
                remote: (*remote).to_owned(),
                ignore: Vec::new(),
            })
            .collect(),
        install,
        restart_on_change: false,
        mode: ServiceMode::default(),
        default_db_branches: false,
        default_split_queues: false,
    }
}

/// `install.on_change` globs are local paths, which `mirrord preview sync` resolved against the
/// directory of `mirrord-up.yaml`, but the preview pod matches them against its own files, so
/// each must become the same glob under the `remote` that holds the files.
#[test]
fn on_change_globs_match_the_pushed_files_in_the_container() {
    let install = InstallConfig {
        command: vec!["npm".to_owned(), "ci".to_owned()],
        on_change: vec![
            "/repo/frontend/package-lock.json".into(),
            "/repo/backend/../frontend/./packages/ui/**/*.json".into(),
            "/repo/frontend/**/requirements*.txt".into(),
        ],
        persist: vec!["/usr/local/lib/node".to_owned()],
    };
    let frontend = service(
        &[("frontend", "/app"), ("frontend/packages/ui", "/ui")],
        Some(install),
    );

    let sync = ServiceSync::of_service(&frontend).unwrap().unwrap();

    assert_eq!(sync.entries, frontend.sync);
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
            run_command: frontend.run.command.clone(),
            working_dir: Some("/app".to_owned()),
            install: Some(PreviewSyncInstall {
                command: vec!["npm".to_owned(), "ci".to_owned()],
                on_change: vec![
                    "/app/package-lock.json".to_owned(),
                    "/ui/**/*.json".to_owned(),
                    "/app/**/requirements*.txt".to_owned(),
                ],
                persist: vec!["/usr/local/lib/node".to_owned()],
            }),
            restart_on_change: false,
        }
    );
}

/// The preview pod only has the pushed files, runs `run.command` itself and has no local process,
/// and one pod gets one push, so a glob outside every `local`, a container run, a `pre_command`,
/// settings that the pod can't mount, an idle start, more than one replica and a CronJob target
/// must fail before anything changes in the cluster.
#[test]
fn settings_that_a_sync_preview_cannot_run_are_refused() {
    let with_glob = |glob: &str| {
        service(
            &[("frontend", "/app")],
            Some(InstallConfig {
                command: vec!["npm".to_owned(), "ci".to_owned()],
                on_change: vec![glob.into()],
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
                Err(CliError::PreviewSyncOnChangeOutsideLocal(refused)) if refused == Path::new(glob)
            ),
            "{glob}"
        );
    }

    let mut container = service(&[("frontend", "/app")], None);
    container.run.r#type = RunType::Container;
    assert!(matches!(
        ServiceSync::of_service(&container),
        Err(CliError::PreviewSyncContainerRun)
    ));

    let mut pre_command = service(&[("frontend", "/app")], None);
    pre_command.run.pre_command = vec!["npm".to_owned(), "run".to_owned(), "build".to_owned()];
    assert!(matches!(
        ServiceSync::of_service(&pre_command),
        Err(CliError::PreviewSyncPreCommand)
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
