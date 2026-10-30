use std::{fs, path::Path, sync::Arc, time::Duration};

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
    sync::SyncFrame,
};
use mirrord_progress::ProgressTracker;
use mirrord_up::{InstallConfig, RunConfig, RunType, ServiceMode, SubprocessCfg, SyncEntry};
use tokio::io::DuplexStream;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

use super::{PreviewSync, ServiceSync, WATCH_DEBOUNCE};
use crate::error::CliError;

/// The operator's end of the sync connection, which answers every push as the sidecar would.
struct Operator {
    socket: WebSocketStream<DuplexStream>,
    generation: u64,
}

impl Operator {
    /// Waits up to `timeout` for the next push, applies it, and returns the entries it named.
    /// Fails when a push starts but doesn't end within `timeout`.
    async fn next_push(&mut self, timeout: Duration) -> Option<Vec<u32>> {
        let message = tokio::time::timeout(timeout, self.socket.next())
            .await
            .ok()?;
        let Message::Binary(bytes) = message.unwrap().unwrap() else {
            panic!("the CLI sent a message that isn't a frame");
        };

        let SyncFrame::Begin {
            push_id, entries, ..
        } = SyncFrame::decode(&bytes).unwrap()
        else {
            panic!("a push starts with `Begin`");
        };

        tokio::time::timeout(timeout, async {
            loop {
                let Message::Binary(bytes) = self.socket.next().await.unwrap().unwrap() else {
                    panic!("the CLI sent a message that isn't a frame");
                };

                match SyncFrame::decode(&bytes).unwrap() {
                    SyncFrame::Data { .. } | SyncFrame::EntryDone { .. } => {}
                    SyncFrame::Commit { .. } => break,
                    frame => panic!("unexpected frame in a push: {frame:?}"),
                }
            }
        })
        .await
        .expect("the push started, but `Commit` didn't arrive");

        self.generation += 1;
        let applied = SyncFrame::Applied {
            push_id,
            generation: self.generation,
            entries: Vec::new(),
        };

        self.socket
            .send(Message::Binary(applied.encode().into()))
            .await
            .unwrap();

        Some(entries.iter().map(|entry| entry.index).collect())
    }
}

/// How long `--watch` gets to push a change: the debounce, packing and the push.
const PUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the test waits to see that `--watch` pushes nothing: the debounce, and as long again
/// for packing and sending the push.
const QUIET_PERIOD: Duration = WATCH_DEBOUNCE.saturating_mul(2);

/// `--watch` pushes an entry once per change to the files that a push carries, including a change
/// made after the first push but before it started watching, and never for what a push doesn't
/// carry, such as a build's output in an ignored directory.
#[tokio::test]
async fn watch_pushes_each_change_to_the_pushed_files_once() {
    let frontend = tempfile::tempdir().unwrap();
    let backend = tempfile::tempdir().unwrap();
    let write = |dir: &Path, path: &str, contents: &str| {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    };

    write(frontend.path(), ".gitignore", "dist/\n");
    write(frontend.path(), "app.js", "v1");
    write(backend.path(), "server.js", "v1");

    let (cli, operator) = tokio::io::duplex(1 << 20);
    let (cli, operator) = tokio::join!(
        WebSocketStream::from_raw_socket(cli, Role::Client, None),
        WebSocketStream::from_raw_socket(operator, Role::Server, None),
    );
    let mut operator = Operator {
        socket: operator,
        generation: 0,
    };
    let locals = Arc::from([frontend.path().to_owned(), backend.path().to_owned()]);

    let (first_push, entries) = tokio::join!(
        PreviewSync::first_push(PreviewSyncConnection::new(cli), locals),
        operator.next_push(PUSH_TIMEOUT),
    );
    let (preview_sync, generation) = first_push.unwrap();
    assert_eq!((generation, entries), (1, Some(vec![0, 1])));

    write(frontend.path(), "app.js", "v2, written before the watch");

    let progress = ProgressTracker::null();
    let edits = async {
        assert_eq!(operator.next_push(PUSH_TIMEOUT).await, Some(vec![0]));

        write(frontend.path(), "app.js", "v3, a longer edit");
        assert_eq!(operator.next_push(PUSH_TIMEOUT).await, Some(vec![0]));
        assert_eq!(
            operator.next_push(QUIET_PERIOD).await,
            None,
            "a push followed although the tree didn't change"
        );

        write(frontend.path(), "dist/bundle.js", "bundle");
        assert_eq!(
            operator.next_push(QUIET_PERIOD).await,
            None,
            "a change to an ignored file started a push"
        );

        write(backend.path(), "server.js", "v2, a longer edit");
        assert_eq!(operator.next_push(PUSH_TIMEOUT).await, Some(vec![1]));
    };

    // `watch` starts watching before it first waits, so polling it first makes it see the edits.
    tokio::select! {
        biased;
        result = preview_sync.watch(&progress) => panic!("`--watch` ended: {result:?}"),
        () = edits => {}
    }
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

    assert_eq!(
        sync.locals,
        [
            Path::new("/repo/frontend"),
            Path::new("/repo/frontend/packages/ui")
        ]
    );
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
