//! Multicluster side of `mirrord preview start`: waiting for the preview's replicas on the
//! non-default clusters via the operator's previews API.

use std::time::Duration;

use kube::Api;
use mirrord_operator::crd::preview::{
    PreviewSessionPhase,
    view::{PreviewMessageKind, PreviewSessionView, PreviewSessionViewStatus},
};
use mirrord_progress::{Progress, ProgressTracker};

/// How long `preview start` waits for the replicas on OTHER clusters after the default
/// cluster's session is `Ready`.
const REPLICA_CLUSTERS_TIMEOUT: Duration = Duration::from_secs(60);

/// Poll cadence of the replica wait.
const REPLICA_CLUSTERS_POLL: Duration = Duration::from_secs(3);

/// Consecutive previews-API failures the replica wait tolerates before giving up WITH a
/// warning. Any single flake used to end the wait as a silent success, hiding real operator
/// errors behind a happy-path summary.
const REPLICA_CLUSTERS_MAX_ERRORS: u32 = 3;

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
/// with a warning, never a silent success. Operators without the previews route (genuine 404
/// on the first fetch) skip the wait silently.
///
/// One [`REPLICA_CLUSTERS_TIMEOUT`] budget covers the WHOLE wait - the initial view fetch and
/// the poll loop share a single deadline, so even a previews API that hangs without erroring
/// cannot stall `preview start` for longer than the budget.
pub(super) async fn wait_for_replica_clusters(
    client: kube::Client,
    namespace: &str,
    session_name: &str,
    progress: &mut ProgressTracker,
) {
    let api = Api::<PreviewSessionView>::namespaced(client, namespace);
    let deadline = tokio::time::Instant::now() + REPLICA_CLUSTERS_TIMEOUT;

    let view = match tokio::time::timeout_at(deadline, first_view(&api, session_name, progress))
        .await
    {
        // Nothing to wait on (older operator, or persistent errors already warned about).
        Ok(None) => return,
        Ok(Some(view)) => view,
        Err(_elapsed) => {
            progress
                .warning("could not read the preview's multicluster status within the wait budget");
            return;
        }
    };

    let Some(status) = view.status else {
        return;
    };
    if let Some(message) = &status.message
        && message.kind == PreviewMessageKind::Degraded
    {
        progress.warning(&message.text);
    }
    if status.clusters.is_empty() {
        return;
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
        poll_replica_clusters(&api, session_name, &mut lagging),
    )
    .await;

    match outcome {
        Ok(ReplicaWait::AllServing(names)) => {
            subtask.success(Some(&format!("preview serving on: {names}")));
            return;
        }
        Ok(ReplicaWait::Failed(message)) => {
            subtask.failure(Some(&format!("the preview failed: {message}")));
            return;
        }
        Ok(ReplicaWait::Deleted) => {
            subtask.warning(
                "the preview was deleted while waiting for its replicas. It may have been \
                 stopped by another `mirrord preview stop`, expired via its TTL, or failed \
                 on one cluster and been cleaned up everywhere. Run `mirrord preview status` \
                 and check the operator logs on the primary cluster to see which.",
            );
        }
        Ok(ReplicaWait::ApiErrors(error)) => {
            subtask.warning(&format!(
                "stopped waiting for replicas - the preview status keeps failing: {error}"
            ));
        }
        Err(_elapsed) => {
            subtask.warning(&format!(
                "some clusters are not serving the preview yet: {}. The operator continues \
                 bringing them up; run `mirrord preview status` to re-check",
                lagging.join(", "),
            ));
        }
    }

    subtask.success(None);
}

/// First fetch of the preview view, tolerating up to [`REPLICA_CLUSTERS_MAX_ERRORS`]
/// consecutive flakes. `None` means there is nothing to wait on: an operator without the
/// previews route (genuine 404), or persistent errors - those warn on `progress` here.
/// The caller bounds this with the wait's shared deadline.
async fn first_view(
    api: &Api<PreviewSessionView>,
    session_name: &str,
    progress: &mut ProgressTracker,
) -> Option<PreviewSessionView> {
    let mut consecutive_errors = 0u32;
    loop {
        match api.get_opt(session_name).await {
            Ok(Some(view)) => return Some(view),
            Ok(None) => return None,
            Err(error) => {
                consecutive_errors += 1;
                if consecutive_errors >= REPLICA_CLUSTERS_MAX_ERRORS {
                    progress.warning(&format!(
                        "could not read the preview's multicluster status: {error}"
                    ));
                    return None;
                }
                tokio::time::sleep(REPLICA_CLUSTERS_POLL).await;
            }
        }
    }
}

/// The unbounded poll loop of [`wait_for_replica_clusters`]; the caller bounds it with the
/// wait's shared deadline. `lagging` is updated on every poll so the timeout case can
/// report which clusters were still converging when time ran out.
async fn poll_replica_clusters(
    api: &Api<PreviewSessionView>,
    session_name: &str,
    lagging: &mut Vec<String>,
) -> ReplicaWait {
    let mut consecutive_errors = 0u32;
    loop {
        let status: PreviewSessionViewStatus = match api.get_opt(session_name).await {
            Ok(Some(view)) => {
                consecutive_errors = 0;
                match view.status {
                    Some(status) => status,
                    // A view without a status is mid-construction; poll again.
                    None => {
                        tokio::time::sleep(REPLICA_CLUSTERS_POLL).await;
                        continue;
                    }
                }
            }
            Ok(None) => return ReplicaWait::Deleted,
            Err(error) => {
                consecutive_errors += 1;
                if consecutive_errors >= REPLICA_CLUSTERS_MAX_ERRORS {
                    return ReplicaWait::ApiErrors(error);
                }
                tokio::time::sleep(REPLICA_CLUSTERS_POLL).await;
                continue;
            }
        };

        if status.phase == PreviewSessionPhase::Failed {
            return ReplicaWait::Failed(
                status
                    .message
                    .map(|message| message.text)
                    .unwrap_or_else(|| "no failure message reported".to_owned()),
            );
        }

        *lagging = status
            .clusters
            .iter()
            .filter(|(_, phase)| !matches!(phase.as_str(), "Ready" | "Idle"))
            .map(|(cluster, phase)| format!("{cluster} ({phase})"))
            .collect();

        if lagging.is_empty() {
            let names = status
                .clusters
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            return ReplicaWait::AllServing(names);
        }

        tokio::time::sleep(REPLICA_CLUSTERS_POLL).await;
    }
}
