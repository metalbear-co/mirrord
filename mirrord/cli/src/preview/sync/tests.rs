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
    sync::{PushMode, RejectReason, SyncFrame, UnpairReason},
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

        loop {
            match self.recv().await {
                SyncFrame::Data { index, chunk } => {
                    archives.entry(index).or_default().extend(chunk);
                }
                SyncFrame::EntryDone { .. } => {}
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

/// Under `--watch`, each change pushes the trees whole again, including a change made while the
/// CLI listed the trees for the push before.
#[tokio::test]
async fn watch_pushes_the_trees_again_on_each_change() {
    let local = tempfile::tempdir().unwrap();
    let dir = local.path();
    write(dir, "app.js", "v1");

    let (sync, mut operators, _activity) = start(vec![entry(dir)], true);
    let mut operator = next_connection(&mut operators).await;
    operator.pair_and_apply(1).await;

    write(dir, "app.js", "v2");
    write(dir, "lib/util.js", "util");
    let push = operator.recv_push().await;
    assert_eq!(push.mode, PushMode::Full);
    assert_eq!(
        push.entries[&0],
        Pushed::from([
            ("app.js".to_owned(), file("v2")),
            ("lib".to_owned(), None),
            ("lib/util.js".to_owned(), file("util")),
        ])
    );

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
/// the CLI pushes the tree whole once it is paired again, and not before.
#[tokio::test]
async fn a_full_push_follows_every_pairing() {
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
    operator.recv_push().await;
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
