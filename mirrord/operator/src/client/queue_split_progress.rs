//! Reports what a starting session's queue split is waiting on.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    ops::Not,
    time::Duration,
};

use kube::{Api, Client, api::ListParams};
use mirrord_config::feature::split_queues::{QueueKind, QueueMode, SplitQueuesConfig};
use mirrord_progress::Progress;
use tokio::time::Instant;

use crate::crd::queue_split::{QueueSplit, QueueSplitHeldQueue, QueueSplitQueue};

const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How long a split may wait before mirrord warns about it.
const WARN_AFTER: Duration = Duration::from_secs(30);

/// `QueueSplitQueue::queue_type` of a Temporal task queue, as the operator labels it.
const TEMPORAL_QUEUE_TYPE: &str = "Temporal";

/// Shows what the queue split owned by one session waits on as a progress task.
///
/// Reads the operator's queue split view, so an operator without it, or a user not allowed to
/// read it, gets no report. Dropping it without [`QueueSplitProgress::finish`] fails the open
/// task.
pub(crate) struct QueueSplitProgress<'a, P: Progress> {
    api: Api<QueueSplit>,
    session: String,
    progress: &'a P,
    task: Option<P>,
    message: Option<String>,
    waiting_since: Option<Instant>,
    warned: bool,
    /// Temporal queue ids the user asked to mirror, see [`mirror_requested`].
    mirror_requested: BTreeSet<String>,
    /// Temporal task queues already warned about, so each is named once.
    warned_temporal: BTreeSet<String>,
    /// Held queues already warned about, by task queue and task kind, so a hold that lasts
    /// across polls is named once.
    warned_held: BTreeSet<HeldQueueKey>,
    available: bool,
}

/// What tells two holds apart: the original task queue and the task kind it holds. The virtual
/// queue a hold waits on can move while the hold lasts, so it is not part of the key.
type HeldQueueKey = (String, String);

impl<'a, P: Progress> QueueSplitProgress<'a, P> {
    pub(crate) fn new(
        client: Client,
        namespace: &str,
        session: String,
        mirror_requested: BTreeSet<String>,
        progress: &'a P,
    ) -> Self {
        Self {
            api: Api::namespaced(client, namespace),
            session,
            progress,
            task: None,
            message: None,
            waiting_since: None,
            warned: false,
            mirror_requested,
            warned_temporal: BTreeSet::new(),
            warned_held: BTreeSet::new(),
            available: true,
        }
    }

    /// Runs `future` while reporting the split.
    pub(crate) async fn run<T, E>(
        mut self,
        future: impl Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        let output = {
            let report = async {
                loop {
                    tokio::time::sleep(POLL_INTERVAL).await;
                    self.poll().await;
                }
            };

            tokio::select! {
                output = future => output,
                _ = report => unreachable!("reporting never ends"),
            }
        };

        if output.is_ok() {
            self.finish().await;
        }

        output
    }

    /// Reads the split once and reports any change in what it waits on.
    pub(crate) async fn poll(&mut self) {
        if !self.available {
            return;
        }

        let splits = match tokio::time::timeout(
            POLL_INTERVAL,
            self.api.list(&ListParams::default()),
        )
        .await
        {
            Ok(Ok(splits)) => splits.items,
            Ok(Err(kube::Error::Api(status))) if matches!(status.code, 403 | 404) => {
                tracing::debug!(
                    ?status,
                    "Queue split view unavailable, not reporting the split"
                );
                self.available = false;

                return;
            }
            Ok(Err(error)) => {
                tracing::debug!(%error, "Failed to read the queue split view");

                return;
            }
            Err(_elapsed) => {
                tracing::debug!("Timed out reading the queue split view");

                return;
            }
        };

        // Every split of the namespace stays in reach: a hold on this session's split can wait
        // on another session's queue, which only that session's split names.
        let listed = splits;
        let splits = listed
            .iter()
            .filter(|split| split.spec.session.eq_ignore_ascii_case(&self.session))
            .collect::<Vec<_>>();
        self.warn_temporal_mirror(&splits);
        self.warn_held_queues(&listed, &splits);

        let message = match SplitWait::of(splits) {
            SplitWait::Pods(message) => message,
            SplitWait::Done => {
                if let Some(mut task) = self.task.take() {
                    task.success(None);
                }
                self.message = None;
                self.waiting_since = None;

                return;
            }
            SplitWait::Unknown => return,
        };

        if self.message.as_ref() != Some(&message) {
            if let Some(mut task) = self.task.take() {
                task.success(None);
            }

            self.task = Some(self.progress.subtask(&message));
        }

        let waiting_since = *self.waiting_since.get_or_insert_with(Instant::now);
        if !self.warned && waiting_since.elapsed() >= WARN_AFTER {
            self.progress
                .warning(&format!("queue splitting is {message}"));
            self.warned = true;
        }

        self.message = Some(message);
    }

    /// Warns once per Temporal task queue the user asked to mirror. Temporal supports `steal`
    /// only, so the operator runs the queue as `steal` and the local worker takes tasks away from
    /// the deployed one; the user should hear that before the first task arrives.
    fn warn_temporal_mirror(&mut self, splits: &[&QueueSplit]) {
        let queues = splits
            .iter()
            .filter_map(|split| split.status.as_ref())
            .flat_map(|status| &status.queues);
        for id in temporal_mirror_requests(&self.mirror_requested, queues) {
            if self.warned_temporal.insert(id.to_owned()) {
                self.progress.warning(&temporal_mirror_warning(id));
            }
        }
    }

    /// Warns once per held task queue and task kind on this session's `splits`. The hold stops
    /// tasks for this session too, and the operator log is the only other place that says so.
    /// `listed` are all the splits the view returned, which name the queue the hold waits on.
    fn warn_held_queues(&mut self, listed: &[QueueSplit], splits: &[&QueueSplit]) {
        for (key, warning) in held_queue_warnings(&self.session, listed, splits) {
            if self.warned_held.insert(key) {
                self.progress.warning(&warning);
            }
        }
    }

    /// Completes the open task, if any, after one last read of the split, so a warning that is
    /// due is printed before the local process starts receiving queue messages.
    ///
    /// The caller finishes once the session is ready, which can happen before the first poll: a
    /// session joining a split that already runs is ready almost at once. Without this read it
    /// would miss the Temporal mirror warning and a held queue that was there from the start.
    pub(crate) async fn finish(mut self) {
        self.poll().await;
        if let Some(mut task) = self.task.take() {
            task.success(None);
        }
    }
}

/// Warnings due for the held queues of `splits`, keyed so the caller prints each hold once.
/// A multi-cluster session has one split per cluster and each reports the same hold, so the
/// key folds them into one warning.
fn held_queue_warnings(
    session: &str,
    listed: &[QueueSplit],
    splits: &[&QueueSplit],
) -> BTreeMap<HeldQueueKey, String> {
    splits
        .iter()
        .filter_map(|split| split.status.as_ref())
        .flat_map(|status| &status.held_queues)
        .map(|held| {
            let key = (held.queue.clone(), held.task_kind.clone());
            let warning = held_queue_warning(held, &full_queue_owner(held, session, listed));
            (key, warning)
        })
        .collect()
}

/// Warning printed when the operator has paused `held.queue`, with `owner` saying whose queue
/// the hold waits on, see [`full_queue_owner`].
fn held_queue_warning(held: &QueueSplitHeldQueue, owner: &str) -> String {
    format!(
        "Temporal task queue `{queue}` is paused for {kind} tasks since {since}: the buffer of \
         {owner} (`{full}`) is full, so no session on this task queue gets new {kind} tasks \
         until that buffer has room again",
        queue = held.queue,
        kind = held.task_kind,
        since = held.since.0,
        full = held.full_queue,
    )
}

/// Describes who reads the virtual queue `held` waits on, so the user knows whose worker has to
/// take tasks: the deployed workload, their own session, or another session. The queue is one
/// of the `tmpQueues` of a split on the same target, and its `scope` and the split's session
/// say whose it is. A queue no listed split names (its session ended between the two reads) is
/// described by name.
fn full_queue_owner(held: &QueueSplitHeldQueue, session: &str, listed: &[QueueSplit]) -> String {
    let owner = listed.iter().find_map(|split| {
        let tmp = split
            .status
            .as_ref()?
            .tmp_queues
            .iter()
            .find(|tmp| tmp.name == held.full_queue)?;
        Some((split, tmp.scope.as_str()))
    });

    match owner {
        Some((_, "workload")) => "the queue the deployed workload reads from".to_owned(),
        Some((split, _)) if split.spec.session.eq_ignore_ascii_case(session) => {
            "your own session's queue".to_owned()
        }
        Some((split, _)) => format!(
            "the queue of session {} of {}",
            split.spec.session, split.spec.owner.username
        ),
        None => "a virtual task queue".to_owned(),
    }
}

/// Warning printed when a Temporal queue the user asked to mirror runs as `steal`.
pub fn temporal_mirror_warning(id: &str) -> String {
    format!(
        "queue `{id}` is a Temporal task queue, and Temporal supports steal only: \
         matching tasks are stolen from the deployed worker instead of mirrored"
    )
}

/// Ids of the Temporal task queues `config` asks to mirror. `mirrord up` in mirror mode sends one
/// `*` entry per broker, and the Temporal one covers every Temporal queue the operator resolves.
/// A `*` entry for another broker says nothing about Temporal queues, so it does not count.
pub(crate) fn mirror_requested(config: &SplitQueuesConfig) -> BTreeSet<String> {
    config
        .of_kind(QueueKind::Temporal)
        .filter(|split| split.queue_mode == QueueMode::Mirror)
        .map(|split| split.queue_id.to_owned())
        .collect()
}

/// Ids of the Temporal task queues in `queues` that `mirror_requested` covers, by exact id or
/// through `*`. These run as `steal`, the only mode Temporal supports.
fn temporal_mirror_requests<'a>(
    mirror_requested: &BTreeSet<String>,
    queues: impl IntoIterator<Item = &'a QueueSplitQueue>,
) -> BTreeSet<&'a str> {
    let all = mirror_requested.contains("*");
    queues
        .into_iter()
        .filter(|queue| queue.queue_type == TEMPORAL_QUEUE_TYPE)
        .filter(|queue| all || mirror_requested.contains(&queue.id))
        .map(|queue| queue.id.as_str())
        .collect()
}

/// What the splits of one session wait on.
#[derive(Debug, PartialEq, Eq)]
enum SplitWait {
    /// Target pods that are not ready, described.
    Pods(String),
    /// Nothing: the splits are not waiting on target pods.
    Done,
    /// No split yet, or one failed.
    Unknown,
}

impl SplitWait {
    /// Reads what `splits` wait on. A multi-cluster session has one split per workload cluster.
    fn of<'a>(splits: impl IntoIterator<Item = &'a QueueSplit>) -> Self {
        let statuses = splits
            .into_iter()
            .filter_map(|split| split.status.as_ref())
            .collect::<Vec<_>>();
        if statuses.is_empty() || statuses.iter().any(|status| status.phase == "Failed") {
            return Self::Unknown;
        }

        let pods = statuses
            .iter()
            .filter(|status| status.phase == "Pending")
            .flat_map(|status| &status.target_pods)
            .filter(|pod| pod.ready.not())
            .map(|pod| {
                let reason = pod.reason.as_deref().unwrap_or("not ready");

                format!("pod `{}`: {reason}", pod.name)
            })
            .collect::<Vec<_>>();
        if pods.is_empty() {
            return Self::Done;
        }

        Self::Pods(format!(
            "waiting for a target pod to restart with the split queues and become ready: {}",
            pods.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use k8s_openapi::{apimachinery::pkg::apis::meta::v1::Time, jiff::Timestamp};
    use mirrord_config::{
        env_key::EnvKey,
        feature::split_queues::{
            QueueKind, QueueMode, QueueSplit as QueueSplitConfig, SplitQueuesConfig,
        },
    };

    use super::{
        HeldQueueKey, SplitWait, held_queue_warnings, mirror_requested, temporal_mirror_requests,
    };
    use crate::crd::{
        queue_split::{
            QueueSplit, QueueSplitHeldQueue, QueueSplitQueue, QueueSplitSpec, QueueSplitStatus,
            QueueSplitTargetPod, QueueSplitTmpQueue,
        },
        session::{KubeResourceTarget, SessionOwner, SessionTarget},
    };

    fn split_of(session: &str, username: &str, status: QueueSplitStatus) -> QueueSplit {
        let mut split = QueueSplit::new(
            &format!("{}.consumer.deployment", session.to_lowercase()),
            QueueSplitSpec {
                session: session.to_owned(),
                target: SessionTarget::KubeResource(KubeResourceTarget {
                    api_version: "apps/v1".to_owned(),
                    kind: "Deployment".to_owned(),
                    name: "consumer".to_owned(),
                    container: "app".to_owned(),
                }),
                owner: SessionOwner {
                    user_id: username.to_owned(),
                    username: username.to_owned(),
                    hostname: "host".to_owned(),
                    k8s_username: username.to_owned(),
                },
                filters: Vec::new(),
            },
        );
        split.status = Some(status);
        split
    }

    fn split(phase: &str, target_pods: Vec<QueueSplitTargetPod>) -> QueueSplit {
        split_of(
            "A1B2",
            "user",
            QueueSplitStatus {
                phase: phase.to_owned(),
                target_pods,
                ..Default::default()
            },
        )
    }

    fn pod(name: &str, ready: bool, reason: Option<&str>) -> QueueSplitTargetPod {
        QueueSplitTargetPod {
            name: name.to_owned(),
            patched: true,
            ready,
            reason: reason.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn pending_split_lists_the_pods_that_are_not_ready() {
        let split = split(
            "Pending",
            vec![
                pod("a", false, Some("container `app` is running but not ready")),
                pod("b", true, None),
                pod("c", false, None),
            ],
        );

        assert_eq!(
            SplitWait::of([&split]),
            SplitWait::Pods(
                "waiting for a target pod to restart with the split queues and become ready: \
                 pod `a`: container `app` is running but not ready; pod `c`: not ready"
                    .to_owned()
            )
        );
    }

    #[test]
    fn splits_on_several_clusters_list_every_pending_pod() {
        let east = split("Pending", vec![pod("a", false, Some("starting"))]);
        let west = split("Pending", vec![pod("b", false, Some("unschedulable"))]);
        let done = split("Ready", vec![pod("c", true, None)]);

        assert_eq!(
            SplitWait::of([&east, &west, &done]),
            SplitWait::Pods(
                "waiting for a target pod to restart with the split queues and become ready: \
                 pod `a`: starting; pod `b`: unschedulable"
                    .to_owned()
            )
        );
    }

    #[test]
    fn split_without_pending_pods_waits_on_nothing() {
        assert_eq!(
            SplitWait::of([&split("Pending", Vec::new())]),
            SplitWait::Done
        );
        assert_eq!(
            SplitWait::of([&split("Ready", vec![pod("a", false, Some("starting"))])]),
            SplitWait::Done
        );
    }

    #[test]
    fn missing_or_failed_split_is_unknown() {
        assert_eq!(SplitWait::of([]), SplitWait::Unknown);
        assert_eq!(
            SplitWait::of([
                &split("Pending", vec![pod("a", false, Some("starting"))]),
                &split("Failed", Vec::new()),
            ]),
            SplitWait::Unknown
        );
    }

    fn resolved(id: &str, queue_type: &str) -> QueueSplitQueue {
        QueueSplitQueue {
            id: id.to_owned(),
            queue_type: queue_type.to_owned(),
            ..Default::default()
        }
    }

    fn ids<const N: usize>(ids: [&str; N]) -> BTreeSet<String> {
        ids.into_iter().map(ToOwned::to_owned).collect()
    }

    /// Only Temporal queues whose mode is `mirror` need a warning: the default `steal` is what
    /// Temporal runs anyway, and a `*` mirror entry for another broker says nothing about
    /// Temporal queues, so neither shows up in the requested set.
    #[test]
    fn mirror_requested_skips_steal_queues() {
        let config = SplitQueuesConfig::from_splits([
            QueueSplitConfig {
                queue_mode: QueueMode::Mirror,
                ..QueueSplitConfig::new("tasks", QueueKind::Temporal)
            },
            QueueSplitConfig::new("orders", QueueKind::Kafka),
            QueueSplitConfig {
                queue_mode: QueueMode::Mirror,
                ..QueueSplitConfig::new("*", QueueKind::Kafka)
            },
        ]);
        assert_eq!(mirror_requested(&config), ids(["tasks"]));

        let key = EnvKey::Provided("session".to_owned());
        let up = SplitQueuesConfig::all_wildcard_with_mode(&key, QueueMode::Mirror);
        assert_eq!(mirror_requested(&up), ids(["*"]));

        let up_steal = SplitQueuesConfig::all_wildcard_with_mode(&key, QueueMode::Steal);
        assert_eq!(mirror_requested(&up_steal), BTreeSet::new());
    }

    /// A Temporal queue the user named with `queue_mode: mirror` is reported; a Temporal queue
    /// left at `steal` and a non-Temporal queue asked to mirror are not.
    #[test]
    fn temporal_queue_requested_by_id_is_reported() {
        let queues = [
            resolved("tasks", "Temporal"),
            resolved("other-tasks", "Temporal"),
            resolved("orders", "Kafka"),
        ];
        assert_eq!(
            temporal_mirror_requests(&ids(["tasks", "orders"]), &queues),
            BTreeSet::from(["tasks"])
        );
    }

    /// `mirrord up` in mirror mode does not know the queue ids and sends `*`, so every Temporal
    /// queue the operator resolved is reported, each by its real id.
    #[test]
    fn wildcard_mirror_request_reports_every_temporal_queue() {
        let queues = [
            resolved("tasks", "Temporal"),
            resolved("other-tasks", "Temporal"),
            resolved("orders", "Kafka"),
        ];
        assert_eq!(
            temporal_mirror_requests(&ids(["*"]), &queues),
            BTreeSet::from(["other-tasks", "tasks"])
        );
    }

    /// A multi-cluster session has one split per cluster, each listing the same queue; the
    /// warning names the queue once.
    #[test]
    fn same_queue_on_several_splits_is_reported_once() {
        let queues = [resolved("tasks", "Temporal"), resolved("tasks", "Temporal")];
        assert_eq!(
            temporal_mirror_requests(&ids(["*"]), &queues),
            BTreeSet::from(["tasks"])
        );
        assert_eq!(
            temporal_mirror_requests(&BTreeSet::new(), &queues),
            BTreeSet::new()
        );
    }

    fn tmp_queue(scope: &str, name: &str) -> QueueSplitTmpQueue {
        QueueSplitTmpQueue {
            id: "orders".to_owned(),
            queue_type: "Temporal".to_owned(),
            kind: "taskQueue".to_owned(),
            scope: scope.to_owned(),
            original: "order-checkout".to_owned(),
            name: name.to_owned(),
        }
    }

    fn held(task_kind: &str, full_queue: &str) -> QueueSplitHeldQueue {
        QueueSplitHeldQueue {
            id: "orders".to_owned(),
            queue_type: "Temporal".to_owned(),
            queue: "order-checkout".to_owned(),
            task_kind: task_kind.to_owned(),
            full_queue: full_queue.to_owned(),
            since: Time(Timestamp::from_second(1_795_860_902).expect("valid timestamp")),
        }
    }

    fn warning<'a>(warnings: &'a BTreeMap<HeldQueueKey, String>, task_kind: &str) -> &'a str {
        warnings
            .get(&("order-checkout".to_owned(), task_kind.to_owned()))
            .expect("the hold is warned about")
    }

    /// A hold is one warning per task queue and task kind, however many polls and clusters
    /// report it, and the warning says whose queue is full: the deployed workload's, resolved
    /// from the workload-scoped `tmpQueues`, or another session's, resolved from that session's
    /// own split, which only the full listing holds.
    #[test]
    fn held_queue_is_warned_once_naming_whose_queue_is_full() {
        let mine = split_of(
            "A1B2",
            "me",
            QueueSplitStatus {
                phase: "Ready".to_owned(),
                tmp_queues: vec![
                    tmp_queue("workload", "mirrord-tmp-a-main-order-checkout"),
                    tmp_queue("session", "mirrord-tmp-a1b2-order-checkout"),
                ],
                held_queues: vec![
                    held("workflow", "mirrord-tmp-a-main-order-checkout"),
                    held("activity", "mirrord-tmp-c3d4-order-checkout"),
                ],
                ..Default::default()
            },
        );
        let theirs = split_of(
            "C3D4",
            "colleague",
            QueueSplitStatus {
                phase: "Ready".to_owned(),
                tmp_queues: vec![tmp_queue("session", "mirrord-tmp-c3d4-order-checkout")],
                held_queues: vec![held("workflow", "mirrord-tmp-a-main-order-checkout")],
                ..Default::default()
            },
        );
        let listed = vec![mine.clone(), theirs];

        // Two clusters report the same split: the key folds them.
        let warnings = held_queue_warnings("a1b2", &listed, &[&mine, &mine]);
        let keys = warnings.keys().cloned().collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                ("order-checkout".to_owned(), "activity".to_owned()),
                ("order-checkout".to_owned(), "workflow".to_owned()),
            ]
        );

        let workflow = warning(&warnings, "workflow");
        assert_eq!(
            workflow,
            "Temporal task queue `order-checkout` is paused for workflow tasks since \
             2026-11-28T10:15:02Z: the buffer of the queue the deployed workload reads from \
             (`mirrord-tmp-a-main-order-checkout`) is full, so no session on this task queue \
             gets new workflow tasks until that buffer has room again"
        );
        let activity = warning(&warnings, "activity");
        assert!(
            activity.contains("the queue of session C3D4 of colleague"),
            "{activity}"
        );
    }

    /// A hold on the session's own queue says so, and a queue no listed split names any more is
    /// described by name rather than dropped.
    #[test]
    fn held_queue_owner_falls_back_to_the_queue_name() {
        let mine = split_of(
            "A1B2",
            "me",
            QueueSplitStatus {
                phase: "Ready".to_owned(),
                tmp_queues: vec![tmp_queue("session", "mirrord-tmp-a1b2-order-checkout")],
                held_queues: vec![
                    held("workflow", "mirrord-tmp-a1b2-order-checkout"),
                    held("activity", "mirrord-tmp-gone-order-checkout"),
                ],
                ..Default::default()
            },
        );
        let listed = vec![mine.clone()];

        let warnings = held_queue_warnings("A1B2", &listed, &[&mine]);
        let workflow = warning(&warnings, "workflow");
        assert!(workflow.contains("your own session's queue"), "{workflow}");
        let activity = warning(&warnings, "activity");
        assert!(
            activity.contains("a virtual task queue (`mirrord-tmp-gone-order-checkout`)"),
            "{activity}"
        );
    }
}
