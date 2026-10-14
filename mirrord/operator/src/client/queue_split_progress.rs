//! Reports what a starting session's queue split is waiting on.

use std::{collections::BTreeSet, future::Future, ops::Not, time::Duration};

use kube::{Api, Client, api::ListParams};
use mirrord_config::feature::split_queues::{QueueMode, SplitQueuesConfig};
use mirrord_progress::Progress;
use tokio::time::Instant;

use crate::crd::queue_split::{QueueSplit, QueueSplitQueue};

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
    /// Queue ids the user asked to mirror, see [`mirror_requested`].
    mirror_requested: BTreeSet<String>,
    /// Temporal task queues already warned about, so each is named once.
    warned_temporal: BTreeSet<String>,
    available: bool,
}

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

        let splits = splits
            .iter()
            .filter(|split| split.spec.session.eq_ignore_ascii_case(&self.session))
            .collect::<Vec<_>>();
        self.warn_temporal_mirror(&splits);

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
                self.progress.warning(&format!(
                    "queue `{id}` is a Temporal task queue, and Temporal supports steal only: \
                     matching tasks are stolen from the deployed worker instead of mirrored"
                ));
            }
        }
    }

    /// Reads the split one last time, then completes the open task, if any.
    ///
    /// The caller finishes once the session is ready, and the local process starts receiving
    /// queue messages right after. A session that becomes ready between two polls would
    /// otherwise skip whatever the split has to say about it, such as the Temporal warning.
    pub(crate) async fn finish(mut self) {
        self.poll().await;
        if let Some(mut task) = self.task.take() {
            task.success(None);
        }
    }
}

/// Ids of the queues `config` asks to mirror. `mirrord up` in mirror mode sends one `*` entry,
/// which covers every queue the operator resolves.
pub(crate) fn mirror_requested(config: &SplitQueuesConfig) -> BTreeSet<String> {
    config
        .queue_modes()
        .filter(|(_, mode)| *mode == QueueMode::Mirror)
        .map(|(id, _)| id.to_owned())
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
    use std::collections::BTreeSet;

    use mirrord_config::{
        env_key::EnvKey,
        feature::split_queues::{
            QueueKind, QueueMode, QueueSplit as QueueSplitConfig, SplitQueuesConfig,
        },
    };

    use super::{SplitWait, mirror_requested, temporal_mirror_requests};
    use crate::crd::{
        queue_split::{
            QueueSplit, QueueSplitQueue, QueueSplitSpec, QueueSplitStatus, QueueSplitTargetPod,
        },
        session::{KubeResourceTarget, SessionOwner, SessionTarget},
    };

    fn split(phase: &str, target_pods: Vec<QueueSplitTargetPod>) -> QueueSplit {
        let mut split = QueueSplit::new(
            "a1b2.consumer.deployment",
            QueueSplitSpec {
                session: "A1B2".to_owned(),
                target: SessionTarget::KubeResource(KubeResourceTarget {
                    api_version: "apps/v1".to_owned(),
                    kind: "Deployment".to_owned(),
                    name: "consumer".to_owned(),
                    container: "app".to_owned(),
                }),
                owner: SessionOwner {
                    user_id: "user".to_owned(),
                    username: "user".to_owned(),
                    hostname: "host".to_owned(),
                    k8s_username: "user".to_owned(),
                },
                filters: Vec::new(),
            },
        );
        split.status = Some(QueueSplitStatus {
            phase: phase.to_owned(),
            target_pods,
            ..Default::default()
        });
        split
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

    /// Only queues whose mode is `mirror` need a warning; the default `steal` is what Temporal
    /// runs anyway, so it must not show up in the requested set.
    #[test]
    fn mirror_requested_skips_steal_queues() {
        let config = SplitQueuesConfig::from_splits([
            QueueSplitConfig {
                queue_mode: QueueMode::Mirror,
                ..QueueSplitConfig::new("tasks", QueueKind::Temporal)
            },
            QueueSplitConfig::new("orders", QueueKind::Kafka),
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
}
