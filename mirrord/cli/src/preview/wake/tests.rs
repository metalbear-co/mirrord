use std::time::Duration;

use futures::{SinkExt, StreamExt};
use mirrord_operator::{
    client::preview_wake::PreviewWakeConnection,
    crd::{
        preview::view::{PreviewEnv, PreviewEnvSpec, PreviewEnvStatus},
        session::{KubeResourceTarget, SessionTarget},
    },
    wake::{ClusterWake, WakeState, WakeStatus},
};
use mirrord_progress::ProgressTracker;
use tokio::{io::DuplexStream, sync::mpsc, task::JoinHandle, time::Instant};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

use super::{
    Connect, PreviewCopy, Verdict, WakeOutcome, WakeTarget, copy_warnings, local_previews_request,
    under_key, wake_all, wake_timeouts,
};
use crate::error::CliError;

/// How long a test waits for something that must happen.
const STEP_TIMEOUT: Duration = Duration::from_secs(10);

/// What a primary reports when it can't reach the operator of a cluster: the whole cause chain.
const CONNECTION_REFUSED: &str = "failed to open the wake route of the cluster's operator: tcp \
                                  connect error: Connection refused (os error 111)";

/// The operator's end of one wake connection, which a test scripts.
struct Operator {
    socket: WebSocketStream<DuplexStream>,
}

impl Operator {
    /// Sends the state of each cluster, which runs a replica unless it is `default`.
    async fn send(&mut self, states: Vec<(Option<&str>, WakeState)>) {
        let status = WakeStatus {
            clusters: states
                .into_iter()
                .map(|(cluster, state)| ClusterWake {
                    replica: cluster.is_some_and(|cluster| cluster != "default"),
                    cluster: cluster.map(ToOwned::to_owned),
                    state,
                })
                .collect(),
        };
        self.socket
            .send(Message::Text(
                serde_json::to_string(&status).unwrap().into(),
            ))
            .await
            .unwrap();
    }

    async fn send_local(&mut self, state: WakeState) {
        self.send(vec![(None, state)]).await;
    }

    /// Resolves once the CLI closed the connection.
    async fn closed(&mut self) {
        tokio::time::timeout(STEP_TIMEOUT, async {
            while let Some(Ok(message)) = self.socket.next().await {
                if let Message::Close(_) = message {
                    break;
                }
            }
        })
        .await
        .expect("the CLI closes the connection");
    }
}

/// A preview whose every connection reaches the test as an [`Operator`] on the returned channel.
fn target(label: &str) -> (WakeTarget<DuplexStream>, mpsc::UnboundedReceiver<Operator>) {
    // Each connection the driver opens --its operator end--> the test
    let (operators_tx, operators_rx) = mpsc::unbounded_channel();
    let connect: Connect<DuplexStream> = Box::new(move || {
        let operators_tx = operators_tx.clone();
        Box::pin(async move {
            let (cli, operator) = tokio::io::duplex(1 << 16);
            let (cli, operator) = tokio::join!(
                WebSocketStream::from_raw_socket(cli, Role::Client, None),
                WebSocketStream::from_raw_socket(operator, Role::Server, None),
            );
            let _ = operators_tx.send(Operator { socket: operator });
            Ok(PreviewWakeConnection::new(cli))
        })
    });

    (
        WakeTarget {
            label: label.to_owned(),
            connect,
        },
        operators_rx,
    )
}

/// Wakes `targets` in the background, waiting up to `timeout`.
fn start(
    targets: Vec<WakeTarget<DuplexStream>>,
    timeout: Duration,
    no_wait: bool,
) -> JoinHandle<WakeOutcome> {
    tokio::spawn(async move {
        wake_all(
            targets,
            Instant::now() + timeout,
            no_wait,
            &ProgressTracker::null(),
        )
        .await
    })
}

async fn next_connection(operators_rx: &mut mpsc::UnboundedReceiver<Operator>) -> Operator {
    tokio::time::timeout(STEP_TIMEOUT, operators_rx.recv())
        .await
        .expect("the CLI connects in time")
        .unwrap()
}

async fn finished(wake: JoinHandle<WakeOutcome>) -> WakeOutcome {
    tokio::time::timeout(STEP_TIMEOUT, wake)
        .await
        .expect("the wake ends in time")
        .unwrap()
}

/// The command waits for every preview to be ready, and holds each one's connection, and with it
/// the preview, until the last one is: a preview that is ready first must not idle while the
/// command waits for another.
#[tokio::test]
async fn waits_for_every_preview_and_holds_them_until_the_end() {
    let (first, mut first_operators_rx) = target("default/first");
    let (second, mut second_operators_rx) = target("default/second");
    let wake = start(vec![first, second], STEP_TIMEOUT, false);

    let mut first = next_connection(&mut first_operators_rx).await;
    let mut second = next_connection(&mut second_operators_rx).await;
    first.send_local(WakeState::Ready).await;
    second.send_local(WakeState::Waking).await;

    // The ready preview stays held while the other one boots.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), first.socket.next())
            .await
            .is_err()
    );
    assert!(!wake.is_finished());

    second.send_local(WakeState::Ready).await;
    assert_eq!(finished(wake).await.failed, Vec::<String>::new());
    first.closed().await;
    second.closed().await;
}

/// A failed preview fails the command, naming the preview and why, but the command still waits
/// for the others.
#[tokio::test]
async fn a_failed_preview_fails_the_command_after_the_others() {
    let (broken, mut broken_operators_rx) = target("default/broken");
    let (fine, mut fine_operators_rx) = target("default/fine");
    let wake = start(vec![broken, fine], STEP_TIMEOUT, false);

    let mut broken = next_connection(&mut broken_operators_rx).await;
    let mut fine = next_connection(&mut fine_operators_rx).await;
    broken
        .send_local(WakeState::Failed {
            message: "image pull failed".to_owned(),
        })
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!wake.is_finished());

    fine.send_local(WakeState::Ready).await;
    assert_eq!(
        finished(wake).await.failed,
        ["default/broken (image pull failed)"]
    );
}

/// A preview that a `mirrord exec` session pauses is skipped, and doesn't fail the command.
#[tokio::test]
async fn a_paused_preview_is_skipped() {
    let (paused, mut operators_rx) = target("default/paused");
    let wake = start(vec![paused], STEP_TIMEOUT, false);

    next_connection(&mut operators_rx)
        .await
        .send_local(WakeState::Paused)
        .await;

    assert_eq!(finished(wake).await.failed, Vec::<String>::new());
}

/// A preview that isn't ready by the deadline fails the command.
#[tokio::test]
async fn a_preview_not_ready_in_time_fails_the_command() {
    let (slow, mut operators_rx) = target("default/slow");
    let wake = start(vec![slow], Duration::from_millis(300), false);

    let mut slow = next_connection(&mut operators_rx).await;
    slow.send_local(WakeState::Waking).await;

    assert_eq!(
        finished(wake).await.failed,
        ["default/slow (not ready in time)"]
    );
    slow.closed().await;
}

/// A connection that breaks, as when the operator restarts, is opened again, so the wait goes on
/// and the operator holds the preview again.
#[tokio::test]
async fn a_broken_connection_is_opened_again() {
    let (preview, mut operators_rx) = target("default/preview");
    let wake = start(vec![preview], STEP_TIMEOUT, false);

    let mut first = next_connection(&mut operators_rx).await;
    first.send_local(WakeState::Waking).await;
    drop(first);

    next_connection(&mut operators_rx)
        .await
        .send_local(WakeState::Ready)
        .await;
    assert_eq!(finished(wake).await.failed, Vec::<String>::new());
}

/// A replica cluster that the primary can't reach, or whose operator can't wake previews, is a
/// warning: the preview counts as ready once every other cluster is.
#[tokio::test]
async fn an_unreachable_replica_cluster_does_not_fail_the_command() {
    let (preview, mut operators_rx) = target("default/preview");
    let wake = start(vec![preview], STEP_TIMEOUT, false);

    next_connection(&mut operators_rx)
        .await
        .send(vec![
            (Some("default"), WakeState::Ready),
            (
                Some("east"),
                WakeState::Unreachable {
                    message: "connection refused".to_owned(),
                },
            ),
            (Some("west"), WakeState::Unsupported),
        ])
        .await;

    let outcome = finished(wake).await;
    assert_eq!(outcome.failed, Vec::<String>::new());
    assert!(
        matches!(
            outcome.warnings.as_slice(),
            [east, west] if east.starts_with("default/preview: cluster `east`")
                && west.starts_with("default/preview: cluster `west`")
        ),
        "{:?}",
        outcome.warnings
    );
}

/// A primary that can't reach the cluster that runs the preview itself keeps trying, so the
/// command waits, and once the deadline passes it fails naming the cluster and why.
#[tokio::test]
async fn an_unreachable_default_cluster_fails_the_command_with_why() {
    let (preview, mut operators_rx) = target("default/preview");
    let wake = start(vec![preview], Duration::from_millis(300), false);

    let mut operator = next_connection(&mut operators_rx).await;
    operator
        .send(vec![
            (
                Some("default"),
                WakeState::Unreachable {
                    message: CONNECTION_REFUSED.to_owned(),
                },
            ),
            (Some("east"), WakeState::Ready),
        ])
        .await;

    assert_eq!(
        finished(wake).await.failed,
        [format!(
            "default/preview (cluster `default`: can't reach the operator to wake the preview: \
             {CONNECTION_REFUSED})"
        )]
    );
}

/// `--no-wait` names each replica cluster that can't be woken, as a wait to the end does: the
/// preview counts as asked to wake, and the command succeeds.
#[tokio::test]
async fn no_wait_names_the_replicas_it_cant_wake() {
    let (preview, mut operators_rx) = target("default/preview");
    let wake = start(vec![preview], STEP_TIMEOUT, true);

    next_connection(&mut operators_rx)
        .await
        .send(vec![
            (Some("default"), WakeState::Waking),
            (Some("east"), WakeState::Unsupported),
            (
                Some("west"),
                WakeState::Unreachable {
                    message: CONNECTION_REFUSED.to_owned(),
                },
            ),
        ])
        .await;

    let outcome = finished(wake).await;
    assert_eq!(outcome.failed, Vec::<String>::new());
    assert!(
        matches!(
            outcome.warnings.as_slice(),
            [east, west] if east.starts_with("default/preview: cluster `east`")
                && west.starts_with("default/preview: cluster `west`")
                && west.contains(CONNECTION_REFUSED)
        ),
        "{:?}",
        outcome.warnings
    );
}

/// A preview whose connection to the operator broke when the deadline passes fails with why, so
/// the developer doesn't take it for a preview that only needs a longer `--timeout`.
#[tokio::test]
async fn a_broken_connection_at_the_deadline_names_why() {
    let (preview, mut operators_rx) = target("default/preview");
    let wake = start(vec![preview], Duration::from_millis(300), false);

    let mut operator = next_connection(&mut operators_rx).await;
    operator.send_local(WakeState::Waking).await;
    drop(operator);

    let failed = finished(wake).await.failed;
    assert!(
        matches!(
            failed.as_slice(),
            [failure] if failure.contains("not ready in time, the connection to the operator broke")
                && failure.contains("the wake connection to the operator failed")
        ),
        "{failed:?}"
    );
}

/// `--no-wait` returns once the operator asked every cluster's preview to wake, without waiting
/// for its pods.
#[tokio::test]
async fn no_wait_returns_once_every_cluster_is_waking() {
    let (preview, mut operators_rx) = target("default/preview");
    let wake = start(vec![preview], STEP_TIMEOUT, true);

    let mut operator = next_connection(&mut operators_rx).await;
    operator
        .send(vec![
            (Some("default"), WakeState::Waking),
            (Some("east"), WakeState::Pending),
        ])
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!wake.is_finished());

    operator
        .send(vec![
            (Some("default"), WakeState::Waking),
            (Some("east"), WakeState::Waking),
        ])
        .await;
    assert_eq!(finished(wake).await.failed, Vec::<String>::new());
}

/// A status the CLI can't read would repeat on every new connection, so it fails the preview.
#[tokio::test]
async fn an_unreadable_status_fails_the_preview() {
    let (preview, mut operators_rx) = target("default/preview");
    let wake = start(vec![preview], STEP_TIMEOUT, false);

    next_connection(&mut operators_rx)
        .await
        .socket
        .send(Message::Text("{\"clusters\": 3}".into()))
        .await
        .unwrap();

    let failed = finished(wake).await.failed;
    assert!(
        matches!(failed.as_slice(), [failure] if failure.starts_with("default/preview (")),
        "{failed:?}"
    );
}

/// A failure on any cluster fails the preview, a pause skips it, and a state from a newer
/// operator keeps the command waiting, rather than counting as ready. A replica cluster that can't
/// be woken is a warning, but the cluster that runs the preview itself is waited for.
#[test]
fn verdict_of_a_status() {
    // `c0` runs the preview itself, the others run replicas.
    let status = |states: Vec<WakeState>| WakeStatus {
        clusters: states
            .into_iter()
            .enumerate()
            .map(|(index, state)| ClusterWake {
                cluster: Some(format!("c{index}")),
                replica: index > 0,
                state,
            })
            .collect(),
    };

    assert_eq!(
        Verdict::of(
            &status(vec![
                WakeState::Ready,
                WakeState::Failed {
                    message: "gone".to_owned()
                },
                WakeState::Paused,
            ]),
            false
        )
        .0,
        Verdict::Failed("cluster `c1`: gone".to_owned())
    );
    assert_eq!(
        Verdict::of(&status(vec![WakeState::Paused, WakeState::Waking]), false).0,
        Verdict::Paused
    );
    assert_eq!(
        Verdict::of(&status(vec![WakeState::Ready, WakeState::Unknown]), false).0,
        Verdict::Waiting {
            triggered: false,
            unreachable: Vec::new()
        }
    );
    assert_eq!(
        Verdict::of(&status(vec![WakeState::Ready, WakeState::Pending]), false).0,
        Verdict::Waiting {
            triggered: false,
            unreachable: Vec::new()
        }
    );
    assert_eq!(
        Verdict::of(&status(Vec::new()), false).0,
        Verdict::Waiting {
            triggered: false,
            unreachable: Vec::new()
        }
    );
    assert!(matches!(
        Verdict::of(&status(vec![
            WakeState::Unreachable {
                message: "connection refused".to_owned()
            },
            WakeState::Ready,
        ]), false).0,
        Verdict::Waiting { triggered: false, unreachable }
            if matches!(
                unreachable.as_slice(),
                [why] if why.contains("`c0`") && why.contains("connection refused")
            )
    ));
    assert!(matches!(
        Verdict::of(&status(vec![WakeState::Ready, WakeState::Unsupported]), false),
        (Verdict::Ready, warnings)
            if matches!(warnings.as_slice(), [warning] if warning.contains("`c1`"))
    ));
}

/// The command lists the previews of every namespace from the operator's `previews` view, asking
/// a multi-cluster primary not to wait on its workload clusters for phases the command never
/// reads.
#[test]
fn previews_are_listed_from_the_local_cluster_only() {
    let request = local_previews_request();

    assert_eq!(request.method(), http::Method::GET);
    assert_eq!(
        request.uri(),
        "/apis/operator.metalbear.co/v1alpha1/previews?localOnly=true&includeCopies=true"
    );
}

/// The view's entry for preview `name` under `key`, a copy of a preview on another cluster when
/// `copy`.
fn view(name: &str, key: &str, copy: bool) -> PreviewEnv {
    let mut view = PreviewEnv::new(
        name,
        PreviewEnvSpec {
            key: key.to_owned(),
            target: SessionTarget::KubeResource(KubeResourceTarget {
                api_version: "apps/v1".to_owned(),
                kind: "Deployment".to_owned(),
                name: "app".to_owned(),
                container: "app".to_owned(),
            }),
            image: "app:dev".to_owned(),
            wake_timeout_secs: None,
        },
    );
    view.metadata.namespace = Some("ns".to_owned());
    view.status = Some(PreviewEnvStatus {
        copy,
        primary_uid: None,
        phase: None,
        message: None,
        clusters: Default::default(),
        logs: Vec::new(),
    });
    view
}

/// In a member cluster's context, the copies under the key are named rather than woken: only
/// the primary's cluster wakes them. Previews under other keys are neither.
#[test]
fn copies_under_the_key_are_set_apart() {
    let (own, copies) = under_key(
        vec![
            view("own", "dev", false),
            view("copy", "dev", true),
            view("other", "prod", false),
        ],
        "dev",
    );

    let names = |previews: &[PreviewEnv]| {
        previews
            .iter()
            .map(|preview| preview.metadata.name.clone().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&own), ["own"]);
    assert_eq!(names(&copies), ["copy"]);
}

/// A copy whose primary the run doesn't wake is named, with where to wake it; one whose primary
/// the run wakes in another context is woken through it, so it gets no warning.
#[test]
fn only_copies_of_primaries_the_run_doesnt_wake_are_named() {
    let copy = |label: &str, primary: &str| PreviewCopy {
        label: label.to_owned(),
        primary_uid: Some(primary.to_owned()),
    };
    let copies = [
        copy("ns/woken in context `member`", "uid-woken"),
        copy("ns/elsewhere in context `member`", "uid-elsewhere"),
    ];

    let warnings = copy_warnings(&copies, &["uid-woken".to_owned()]);

    assert!(
        matches!(
            warnings.as_slice(),
            [warning] if warning.contains("ns/elsewhere in context `member`")
                && warning.contains("primary's cluster")
        ),
        "{warnings:?}"
    );
    assert_eq!(copy_warnings(&copies, &[]).len(), 2);
}

/// A `mirrord exec` session that pauses a replica copy leaves the preview to wake: the command
/// names the paused replica, keeps holding the preview, and succeeds once the cluster that runs
/// it is ready.
#[tokio::test]
async fn a_paused_replica_is_a_warning() {
    let (preview, mut operators_rx) = target("default/preview");
    let wake = start(vec![preview], STEP_TIMEOUT, false);

    let mut operator = next_connection(&mut operators_rx).await;
    operator
        .send(vec![
            (Some("default"), WakeState::Waking),
            (Some("east"), WakeState::Paused),
        ])
        .await;
    // The preview is still held: the connection stays open.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), operator.socket.next())
            .await
            .is_err()
    );
    assert!(!wake.is_finished());

    operator
        .send(vec![
            (Some("default"), WakeState::Ready),
            (Some("east"), WakeState::Paused),
        ])
        .await;
    let outcome = finished(wake).await;
    assert_eq!(outcome.failed, Vec::<String>::new());
    assert!(
        matches!(
            outcome.warnings.as_slice(),
            [warning] if warning.starts_with("default/preview: cluster `east`")
                && warning.contains("mirrord exec")
        ),
        "{:?}",
        outcome.warnings
    );
}

/// A replica whose connection the operator re-establishes is waited for, as the preview may not be
/// ready on it yet, and is only a warning once the deadline passes, as an unreachable replica is.
/// The cluster that runs the preview itself is waited for, and fails the preview with why.
#[test]
fn a_reconnecting_cluster_is_waited_for() {
    let reconnecting = || WakeState::Reconnecting {
        message: "lost the wake connection to the cluster's operator: the connection closed"
            .to_owned(),
    };
    let status = |states: Vec<(bool, WakeState)>| WakeStatus {
        clusters: states
            .into_iter()
            .enumerate()
            .map(|(index, (replica, state))| ClusterWake {
                cluster: Some(format!("c{index}")),
                replica,
                state,
            })
            .collect(),
    };

    let replica = status(vec![(false, WakeState::Ready), (true, reconnecting())]);
    assert_eq!(
        Verdict::of(&replica, false),
        (
            Verdict::Waiting {
                triggered: false,
                unreachable: Vec::new()
            },
            Vec::new()
        )
    );
    assert!(matches!(
        Verdict::of(&replica, true),
        (Verdict::Ready, warnings)
            if matches!(warnings.as_slice(), [warning] if warning.contains("`c1`")
                && warning.contains("the connection closed"))
    ));

    let default = status(vec![(false, reconnecting())]);
    assert!(matches!(
        Verdict::of(&default, true).0,
        Verdict::Waiting { triggered: false, unreachable }
            if matches!(unreachable.as_slice(), [why] if why.contains("`c0`")
                && why.contains("the connection closed"))
    ));
}

/// A replica that is still reconnecting when the deadline passes is a warning: the command
/// succeeds once the cluster that runs the preview is ready, but not before the deadline.
#[tokio::test]
async fn a_replica_reconnecting_at_the_deadline_is_a_warning() {
    let (preview, mut operators_rx) = target("default/preview");
    let wake = start(vec![preview], Duration::from_millis(300), false);

    // Kept open: the command's own connection holds the preview until the deadline.
    let mut operator = next_connection(&mut operators_rx).await;
    operator
        .send(vec![
            (Some("default"), WakeState::Ready),
            (
                Some("east"),
                WakeState::Reconnecting {
                    message: "the connection closed".to_owned(),
                },
            ),
        ])
        .await;

    let outcome = finished(wake).await;
    assert_eq!(outcome.failed, Vec::<String>::new());
    assert!(
        matches!(
            outcome.warnings.as_slice(),
            [warning] if warning.starts_with("default/preview: cluster `east`")
        ),
        "{:?}",
        outcome.warnings
    );
}

/// The deadline judges a replica that is still reconnecting as a warning only while the command's
/// own connection to the operator holds the preview: once that connection broke, the last status
/// no longer holds, and the preview fails, naming the broken connection.
#[tokio::test]
async fn a_stale_status_is_not_judged_ready_at_the_deadline() {
    let (preview, mut operators_rx) = target("default/preview");
    let wake = start(vec![preview], Duration::from_millis(300), false);

    let mut operator = next_connection(&mut operators_rx).await;
    operator
        .send(vec![
            (Some("default"), WakeState::Ready),
            (
                Some("east"),
                WakeState::Reconnecting {
                    message: "the connection closed".to_owned(),
                },
            ),
        ])
        .await;
    drop(operator);

    let failed = finished(wake).await.failed;
    assert!(
        matches!(
            failed.as_slice(),
            [failure] if failure.contains("the connection to the operator broke")
        ),
        "{failed:?}"
    );
}

/// When the command's own connection breaks after the operator reported the cluster that runs the
/// preview unreachable, the deadline names both causes.
#[tokio::test]
async fn a_broken_connection_after_an_unreachable_default_cluster_names_both() {
    let (preview, mut operators_rx) = target("default/preview");
    let wake = start(vec![preview], Duration::from_millis(300), false);

    let mut operator = next_connection(&mut operators_rx).await;
    operator
        .send(vec![(
            Some("default"),
            WakeState::Unreachable {
                message: CONNECTION_REFUSED.to_owned(),
            },
        )])
        .await;
    drop(operator);

    let failed = finished(wake).await.failed;
    assert!(
        matches!(
            failed.as_slice(),
            [failure] if failure.contains(CONNECTION_REFUSED)
                && failure.contains("the connection to the operator broke")
        ),
        "{failed:?}"
    );
}

/// An operator that doesn't report its default wake timeout fails the command only when a
/// preview needs it: one without a timeout of its own, and no `--timeout`. With no preview under
/// the key, or previews that all set their own, the command goes on.
#[test]
fn the_operator_default_is_needed_only_for_a_preview_without_its_own() {
    let own = Duration::from_secs(120);

    assert_eq!(
        wake_timeouts([], None, true).unwrap(),
        Vec::<Duration>::new()
    );
    assert_eq!(
        wake_timeouts([Some(120), Some(120)], None, true).unwrap(),
        [own, own]
    );
    assert!(matches!(
        wake_timeouts([Some(120), None], None, true),
        Err(CliError::PreviewWakeTimeoutUnknown)
    ));
    assert_eq!(wake_timeouts([None], None, false).unwrap().len(), 1);
    assert_eq!(
        wake_timeouts([None], Some(Duration::from_secs(90)), true).unwrap(),
        [Duration::from_secs(90)]
    );
}

/// `--no-wait` doesn't take a state from a newer operator for a wake asked for: it waits for a
/// state it knows.
#[tokio::test]
async fn no_wait_waits_past_an_unknown_state() {
    let (preview, mut operators_rx) = target("default/preview");
    let wake = start(vec![preview], STEP_TIMEOUT, true);

    let mut operator = next_connection(&mut operators_rx).await;
    operator.send_local(WakeState::Unknown).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!wake.is_finished());

    operator.send_local(WakeState::Waking).await;
    assert_eq!(finished(wake).await.failed, Vec::<String>::new());
}

/// A preview that fails after it was ready fails the command, naming it, though the command
/// already counted it ready while another preview still woke.
#[tokio::test]
async fn a_ready_preview_that_fails_later_fails_the_command() {
    let (first, mut first_operators_rx) = target("default/first");
    let (second, mut second_operators_rx) = target("default/second");
    let wake = start(vec![first, second], STEP_TIMEOUT, false);

    let mut first = next_connection(&mut first_operators_rx).await;
    let mut second = next_connection(&mut second_operators_rx).await;
    first.send_local(WakeState::Ready).await;
    second.send_local(WakeState::Waking).await;
    first
        .send_local(WakeState::Failed {
            message: "the preview was deleted".to_owned(),
        })
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    second.send_local(WakeState::Ready).await;

    assert_eq!(
        finished(wake).await.failed,
        ["default/first (the preview was deleted)"]
    );
}

/// A preview that a `mirrord exec` session pauses after it was ready is skipped, with a warning
/// naming it.
#[tokio::test]
async fn a_ready_preview_paused_later_is_skipped() {
    let (first, mut first_operators_rx) = target("default/first");
    let (second, mut second_operators_rx) = target("default/second");
    let wake = start(vec![first, second], STEP_TIMEOUT, false);

    let mut first = next_connection(&mut first_operators_rx).await;
    let mut second = next_connection(&mut second_operators_rx).await;
    first.send_local(WakeState::Ready).await;
    second.send_local(WakeState::Waking).await;
    first.send_local(WakeState::Paused).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    second.send_local(WakeState::Ready).await;

    let outcome = finished(wake).await;
    assert_eq!(outcome.failed, Vec::<String>::new());
    assert!(
        matches!(
            outcome.warnings.as_slice(),
            [warning] if warning.contains("skipping default/first")
        ),
        "{:?}",
        outcome.warnings
    );
}

/// A ready preview that the operator reports waking again, as when its pods restart, is waited
/// for again until it is ready.
#[tokio::test]
async fn a_ready_preview_that_wakes_again_is_waited_for() {
    let (first, mut first_operators_rx) = target("default/first");
    let (second, mut second_operators_rx) = target("default/second");
    let wake = start(vec![first, second], STEP_TIMEOUT, false);

    let mut first = next_connection(&mut first_operators_rx).await;
    let mut second = next_connection(&mut second_operators_rx).await;
    first.send_local(WakeState::Ready).await;
    second.send_local(WakeState::Waking).await;
    first.send_local(WakeState::Waking).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    second.send_local(WakeState::Ready).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!wake.is_finished());

    first.send_local(WakeState::Ready).await;
    assert_eq!(finished(wake).await.failed, Vec::<String>::new());
}

/// A break of a ready preview's connection is no news: the operator sends a new connection's
/// first status only once it observed the preview, so a Ready there keeps it ready.
#[tokio::test]
async fn a_ready_preview_stays_ready_across_a_reconnect() {
    let (first, mut first_operators_rx) = target("default/first");
    let (second, mut second_operators_rx) = target("default/second");
    let wake = start(vec![first, second], STEP_TIMEOUT, false);

    let mut first = next_connection(&mut first_operators_rx).await;
    let mut second = next_connection(&mut second_operators_rx).await;
    first.send_local(WakeState::Ready).await;
    second.send_local(WakeState::Waking).await;
    drop(first);

    let mut first = next_connection(&mut first_operators_rx).await;
    first.send_local(WakeState::Ready).await;
    second.send_local(WakeState::Ready).await;

    assert_eq!(finished(wake).await.failed, Vec::<String>::new());
}

/// A ready preview whose new connection hasn't reported when the deadline passes counts as it
/// stood: its pods stayed up, and only the command's hold on them broke.
#[tokio::test]
async fn a_ready_preview_still_reconnecting_at_the_deadline_is_ready() {
    let (first, mut first_operators_rx) = target("default/first");
    let (second, mut second_operators_rx) = target("default/second");
    let wake = start(vec![first, second], Duration::from_millis(300), false);

    let mut first = next_connection(&mut first_operators_rx).await;
    let mut second = next_connection(&mut second_operators_rx).await;
    first.send_local(WakeState::Ready).await;
    second.send_local(WakeState::Waking).await;
    drop(first);

    assert_eq!(
        finished(wake).await.failed,
        ["default/second (not ready in time)"]
    );
}

/// A cluster whose operator is connected but hasn't reported the copy yet is waited for, as a
/// reconnecting one is: a replica's is only a warning at the deadline, naming the cluster and why,
/// and the cluster that runs the preview itself fails it with why.
#[test]
fn a_cluster_not_reported_yet_is_waited_for() {
    let not_reported = || WakeState::NotReported {
        message: "the cluster's operator reported nothing about the copy within 30s".to_owned(),
    };
    let status = |states: Vec<(bool, WakeState)>| WakeStatus {
        clusters: states
            .into_iter()
            .enumerate()
            .map(|(index, (replica, state))| ClusterWake {
                cluster: Some(format!("c{index}")),
                replica,
                state,
            })
            .collect(),
    };

    let replica = status(vec![(false, WakeState::Ready), (true, not_reported())]);
    assert_eq!(
        Verdict::of(&replica, false),
        (
            Verdict::Waiting {
                triggered: false,
                unreachable: Vec::new()
            },
            Vec::new()
        )
    );
    assert!(matches!(
        Verdict::of(&replica, true),
        (Verdict::Ready, warnings)
            if matches!(warnings.as_slice(), [warning] if warning.contains("`c1`")
                && warning.contains("hasn't reported the replica")
                && warning.contains("within 30s"))
    ));

    let default = status(vec![(false, not_reported())]);
    assert!(matches!(
        Verdict::of(&default, true).0,
        Verdict::Waiting { triggered: false, unreachable }
            if matches!(unreachable.as_slice(), [why] if why.contains("`c0`")
                && why.contains("hasn't reported the preview"))
    ));
}
