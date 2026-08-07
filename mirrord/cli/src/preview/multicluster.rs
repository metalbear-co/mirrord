//! Multicluster side of `mirrord preview start`: waiting for the preview's replicas on the
//! non-default clusters via the operator's previews API.

use std::{collections::HashMap, time::Duration};

use futures::{StreamExt, stream::FuturesUnordered};
use kube::Api;
use mirrord_operator::crd::preview::{
    PreviewSession, PreviewSessionPhase,
    view::{
        PreviewMessageKind, PreviewSessionView, PreviewSessionViewPhase, PreviewSessionViewStatus,
    },
};
use mirrord_progress::{Progress, ProgressTracker};
use tokio_retry::{
    Retry,
    strategy::{ExponentialBackoff, jitter},
};

/// How long `preview start` waits for the replicas on OTHER clusters after the default
/// cluster's session is `Ready`.
const REPLICA_CLUSTERS_TIMEOUT: Duration = Duration::from_secs(60);

/// Poll cadence of the replica wait.
const REPLICA_CLUSTERS_POLL: Duration = Duration::from_secs(3);

/// Consecutive previews-API failures one fetch tolerates before giving up WITH a warning.
/// Any single flake used to end the wait as a silent success, hiding real operator errors
/// behind a happy-path summary.
const REPLICA_CLUSTERS_MAX_ERRORS: usize = 3;

/// Retry strategy for one previews-API fetch (same shape as the intproxy's agent-connection
/// retries): 1s then 2s, capped at the poll cadence so the retries never outpace the budget
/// set by [`REPLICA_CLUSTERS_TIMEOUT`].
fn fetch_retries() -> impl Iterator<Item = Duration> {
    ExponentialBackoff::from_millis(2)
        .factor(500)
        .max_delay(REPLICA_CLUSTERS_POLL)
        .take(REPLICA_CLUSTERS_MAX_ERRORS - 1)
}

/// What [`wait_for_replica_clusters`] leaves behind for `preview start` to report.
pub(super) enum ReplicaOutcome {
    /// The preview exists: serving, still converging, or unobservable - all cases where
    /// reporting a successful start is honest.
    Live,
    /// The preview failed while the wait watched it; carries the failure text.
    Failed(String),
    /// The preview disappeared while the wait watched it. Reporting success for either of
    /// these would hand the user a key, namespace and session name for something that is
    /// gone.
    Deleted,
}

/// How the bounded replica wait ended; the caller translates this into progress output.
/// Timeout is not represented here - it surfaces as [`tokio::time::error::Elapsed`] from the
/// [`tokio::time::timeout_at`] bounding the poll loop with the wait's shared deadline.
enum ReplicaWait {
    /// Every cluster reports `Ready` or `Idle`; carries the joined cluster names.
    AllServing(String),
    /// The preview reached `Failed`; carries the failure text.
    Failed(String),
    /// The preview view disappeared mid-wait.
    Deleted,
    /// The previews API kept erroring; carries the last error.
    ApiErrors(kube::Error),
}

/// Waits until every workload cluster reports the preview `Ready` (or `Idle`) - the default
/// cluster's main session and the other clusters' replica copies alike - polling the
/// operator's previews API — the primary aggregates each cluster's copy phase live, so this is
/// the only place the CLI can see all clusters without holding their credentials.
///
/// Deliberately best-effort for SLOW clusters: replicas exist for availability, so a cluster
/// that lags must not fail (or block forever) the whole start - on timeout the lagging
/// clusters are named and the command proceeds. But not blind: a preview that FAILS while
/// waiting surfaces immediately with its failure message, a degraded fleet (replicas off,
/// credential unavailable) is announced instead of waited on, and persistent API errors end
/// with a warning, never a silent success. A preview the operator reports as `NotFound` skips
/// the wait silently; an operator too old to serve the route at all answers with a body no
/// Kubernetes client can parse, which surfaces as a warned-about error rather than a hang.
///
/// One [`REPLICA_CLUSTERS_TIMEOUT`] budget covers the WHOLE wait - the initial view fetch and
/// the poll loop share a single deadline, so even a previews API that hangs without erroring
/// cannot stall `preview start` for longer than the budget.
pub(super) async fn wait_for_replica_clusters(
    client: kube::Client,
    namespace: &str,
    session_name: &str,
    progress: &mut ProgressTracker,
) -> ReplicaOutcome {
    let api = Api::<PreviewSessionView>::namespaced(client, namespace);
    let deadline = tokio::time::Instant::now() + REPLICA_CLUSTERS_TIMEOUT;

    let view = match tokio::time::timeout_at(deadline, first_view(&api, session_name, progress))
        .await
    {
        // Nothing to wait on (older operator, or persistent errors already warned about).
        Ok(None) => return ReplicaOutcome::Live,
        Ok(Some(view)) => view,
        Err(_elapsed) => {
            progress
                .warning("could not read the preview's multicluster status within the wait budget");
            return ReplicaOutcome::Live;
        }
    };

    let Some(status) = view.status else {
        return ReplicaOutcome::Live;
    };
    if let Some(message) = &status.message
        && message.kind == PreviewMessageKind::Degraded
    {
        progress.warning(&message.text);
    }
    if status.clusters.is_empty() {
        return ReplicaOutcome::Live;
    }

    let mut subtask = progress.subtask(&format!(
        "waiting for the preview on {} cluster(s)",
        status.clusters.len()
    ));

    // The last lagging set outlives the timed-out poll future so the timeout warning can
    // name the clusters that were still converging.
    let mut lagging = Vec::new();
    let outcome = tokio::time::timeout_at(
        deadline,
        poll_replica_clusters(&api, session_name, status, &mut lagging),
    )
    .await;

    match outcome {
        Ok(ReplicaWait::AllServing(names)) => {
            subtask.success(Some(&format!("preview serving on: {names}")));
            ReplicaOutcome::Live
        }
        Ok(ReplicaWait::Failed(message)) => {
            subtask.failure(Some(&format!("the preview failed: {message}")));
            ReplicaOutcome::Failed(message)
        }
        Ok(ReplicaWait::Deleted) => {
            subtask.failure(Some(
                "the preview was deleted while waiting for its replicas. It may have been \
                 stopped by another `mirrord preview stop`, expired via its TTL, or failed \
                 on one cluster and been cleaned up everywhere. Run `mirrord preview status` \
                 and check the operator logs on the primary cluster to see which.",
            ));
            ReplicaOutcome::Deleted
        }
        Ok(ReplicaWait::ApiErrors(error)) => {
            subtask.warning(&format!(
                "stopped waiting for replicas - the preview status keeps failing: {error}"
            ));
            subtask.success(None);
            ReplicaOutcome::Live
        }
        Err(_elapsed) => {
            subtask.warning(&format!(
                "some clusters are not serving the preview yet: {}. The operator continues \
                 bringing them up; run `mirrord preview status` to re-check",
                lagging.join(", "),
            ));
            subtask.success(None);
            ReplicaOutcome::Live
        }
    }
}

/// First fetch of the preview view, retrying flakes per [`fetch_retries`]. `None` means
/// there is nothing to wait on: an operator without the previews route (genuine 404), or
/// persistent errors - those warn on `progress` here. The caller bounds this with the
/// wait's shared deadline.
async fn first_view(
    api: &Api<PreviewSessionView>,
    session_name: &str,
    progress: &mut ProgressTracker,
) -> Option<PreviewSessionView> {
    match Retry::start(fetch_retries(), || api.get_opt(session_name)).await {
        Ok(view) => view,
        Err(error) => {
            progress.warning(&format!(
                "could not read the preview's multicluster status: {error}"
            ));
            None
        }
    }
}

/// The unbounded poll loop of [`wait_for_replica_clusters`]; the caller bounds it with the
/// wait's shared deadline and passes the status it already fetched, so a fleet that has
/// already converged returns without a second round trip. `lagging` is updated on every poll
/// so the timeout case can report which clusters were still converging when time ran out.
async fn poll_replica_clusters(
    api: &Api<PreviewSessionView>,
    session_name: &str,
    first: PreviewSessionViewStatus,
    lagging: &mut Vec<String>,
) -> ReplicaWait {
    let mut current = Some(first);

    loop {
        if let Some(status) = current.take()
            && let Some(outcome) = evaluate(&status, lagging)
        {
            return outcome;
        }

        tokio::time::sleep(REPLICA_CLUSTERS_POLL).await;

        current = match Retry::start(fetch_retries(), || api.get_opt(session_name)).await {
            // A view without a status is mid-construction; poll again.
            Ok(Some(view)) => view.status,
            Ok(None) => return ReplicaWait::Deleted,
            Err(error) => return ReplicaWait::ApiErrors(error),
        };
    }
}

/// Decides the wait from one status snapshot. `None` means keep polling, and `lagging` then
/// names the clusters still converging.
fn evaluate(status: &PreviewSessionViewStatus, lagging: &mut Vec<String>) -> Option<ReplicaWait> {
    if status.phase == Some(PreviewSessionPhase::Failed) {
        return Some(ReplicaWait::Failed(
            status
                .message
                .as_ref()
                .map(|message| message.text.clone())
                .unwrap_or_else(|| "no failure message reported".to_owned()),
        ));
    }

    *lagging = status
        .clusters
        .iter()
        .filter(|(_, cluster)| {
            !matches!(
                cluster.phase,
                PreviewSessionViewPhase::Active(
                    PreviewSessionPhase::Ready | PreviewSessionPhase::Idle
                )
            )
        })
        .map(|(cluster, status)| format!("{cluster} ({})", status.phase))
        .collect();

    lagging.is_empty().then(|| {
        ReplicaWait::AllServing(
            status
                .clusters
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        )
    })
}

/// Per-session view fetch for `mirrord preview status`. Every session is fetched at once, so
/// a jittered backoff keeps a rate-limiting apiserver from silently dropping the multicluster
/// detail; the attempts stay few because a missing route (older operator) is the common case.
fn status_retries() -> impl Iterator<Item = Duration> {
    ExponentialBackoff::from_millis(2)
        .factor(100)
        .max_delay(Duration::from_secs(1))
        .map(jitter)
        .take(2)
}

/// Fetches the multicluster view of every listed session CONCURRENTLY.
///
/// The detail is per session and lives behind its own previews-API call, so fetching in
/// sequence made `preview status` as slow as the sum of its sessions - painful exactly when
/// one cluster is unreachable and every call waits. Best-effort throughout: a session whose
/// view is missing or erroring is simply absent from the map, and prints without the detail.
pub(super) async fn cluster_views(
    client: &kube::Client,
    sessions: &[&PreviewSession],
) -> HashMap<(String, String), PreviewSessionViewStatus> {
    sessions
        .iter()
        .filter_map(|session| {
            let name = session.metadata.name.clone()?;
            let namespace = session.metadata.namespace.clone()?;
            Some((namespace, name))
        })
        .map(|(namespace, name)| {
            let api = Api::<PreviewSessionView>::namespaced(client.clone(), &namespace);
            async move {
                let status = Retry::start(status_retries(), || api.get_opt(&name))
                    .await
                    .ok()
                    .flatten()
                    .and_then(|view| view.status)?;
                Some(((namespace, name), status))
            }
        })
        .collect::<FuturesUnordered<_>>()
        .filter_map(std::future::ready)
        .collect()
        .await
}
