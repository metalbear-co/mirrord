//! `mirrord preview wake`: wakes every preview environment under a key, in the kube contexts that
//! `mirrord preview down` deletes them in (see [`up::key_scopes`]), and waits until each one's
//! pods are ready.
//!
//! The CLI lists the previews of every context first, so that one deadline covers them all, then
//! opens each preview's `previews/{name}/wake` websocket at once. The operator wakes the preview
//! and reports its state on every cluster it runs on (see [`mirrord_operator::wake`]), and holds it
//! awake for as long as the connection lasts. The CLI keeps every connection open until the
//! command ends, so no preview idles again while it waits for the others. A connection that
//! breaks, as when the operator restarts, is opened again until the deadline.

use std::{future::Future, ops::Not, pin::Pin, sync::Arc, time::Duration};

use futures::future::join_all;
use http::Request;
use kube::{Resource, core::ObjectList};
use mirrord_analytics::{AnalyticsReporter, ExecutionKind};
use mirrord_operator::{
    client::{
        error::OperatorApiError,
        preview_sync::UpgradedConnection,
        preview_wake::{PreviewWakeConnection, PreviewWakeError},
    },
    crd::{
        NewOperatorFeature,
        preview::{PreviewIdleConfig, view::PreviewEnv},
    },
    wake::{WakeState, WakeStatus},
};
use mirrord_progress::{Progress, ProgressTracker};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
    time::Instant,
};
use tokio_util::task::AbortOnDropHandle;
use tracing::Level;

use super::{
    create_preview_api, deadline_after, load_preview_config,
    sync::{MAX_RECONNECT_DELAY, RECONNECT_DELAY, refused, with_causes},
    up,
};
use crate::{
    config::{PreviewCommonArgs, PreviewWakeArgs},
    data::UserData,
    error::{CliError, CliResult},
};

#[cfg(test)]
mod tests;

/// Opens the wake websocket of one preview, as many times as the wake needs one.
type Connect<S> = Box<
    dyn FnMut() -> Pin<
            Box<dyn Future<Output = Result<PreviewWakeConnection<S>, OperatorApiError>> + Send>,
        > + Send,
>;

/// One preview to wake.
struct WakeTarget<S> {
    /// How the command names the preview.
    label: String,

    connect: Connect<S>,
}

/// The previews of one kube context.
struct ScopePreviews {
    targets: Vec<WakeTarget<UpgradedConnection>>,

    /// How long each preview waits for its pods when it wakes.
    wake_timeouts: Vec<Duration>,

    /// The uid of each preview of `targets`, which a copy in another context can name as its
    /// primary.
    uids: Vec<String>,

    /// The copies under the key whose primary runs on another cluster.
    copies: Vec<PreviewCopy>,
}

/// A copy of a multi-cluster preview whose primary runs on another cluster, which only the
/// primary's cluster wakes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviewCopy {
    /// How the command names the copy.
    label: String,

    /// The uid of the copy's primary, when the operator reports it.
    primary_uid: Option<String>,
}

/// Handle `mirrord preview wake`.
#[mirrord_macros::boxed_future]
#[tracing::instrument(level = Level::TRACE, ret, skip_all)]
pub(super) async fn preview_wake(
    common: &PreviewCommonArgs,
    args: PreviewWakeArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    let scopes = up::key_scopes(common).await?;
    let mut progress = ProgressTracker::from_env("mirrord preview wake");

    let several = scopes.len() > 1;
    let found = join_all(scopes.iter().map(|scope| {
        find_previews(
            scope,
            several,
            args.timeout.is_none(),
            &progress,
            watch.clone(),
            user_data,
        )
    }))
    .await;

    let mut first_error = None;
    let mut targets = Vec::new();
    let mut wake_timeouts = Vec::new();
    let mut uids = Vec::new();
    let mut copies = Vec::new();
    for result in found {
        match result {
            Ok(scope) => {
                targets.extend(scope.targets);
                wake_timeouts.extend(scope.wake_timeouts);
                uids.extend(scope.uids);
                copies.extend(scope.copies);
            }
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }

    for warning in copy_warnings(&copies, &uids) {
        progress.warning(&warning);
    }

    if targets.is_empty() {
        return match first_error {
            Some(error) => {
                progress.failure(None);
                Err(error)
            }
            None => {
                progress.success(Some("no preview environments to wake under the key"));
                Ok(())
            }
        };
    }

    let timeout = args.timeout.map(Duration::from_secs).unwrap_or_else(|| {
        wake_timeouts
            .into_iter()
            .max()
            .expect("every target has a wake timeout")
    });
    let WakeOutcome { failed, warnings } =
        wake_all(targets, deadline_after(timeout), args.no_wait, &progress).await;
    for warning in warnings {
        progress.warning(&warning);
    }

    if let Some(error) = first_error {
        progress.failure(None);
        return Err(error);
    }
    if failed.is_empty().not() {
        progress.failure(None);
        return Err(CliError::PreviewWakeFailed(failed));
    }

    progress.success(None);
    Ok(())
}

/// Lists the previews of `scope`'s key in all namespaces of `scope`'s context, and how long each
/// one waits for its pods when it wakes, for which the operator's default is needed only when
/// `default_timeout` is, without `--timeout`. Prefixes each preview's label with the context when
/// there are `several`.
async fn find_previews(
    scope: &PreviewCommonArgs,
    several: bool,
    default_timeout: bool,
    progress: &ProgressTracker,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<ScopePreviews> {
    let context = scope.context.as_deref();
    let mut progress = match (several, context) {
        (false, _) => progress.subtask("finding preview environments"),
        (true, Some(context)) => progress.subtask(&format!(
            "finding preview environments in context `{context}`"
        )),
        (true, None) => progress.subtask("finding preview environments in the default context"),
    };

    let layer_config = load_preview_config(scope.as_env_vars(), true, &mut progress).await?;
    let key = layer_config
        .key
        .provided()
        .ok_or(CliError::SessionKeyRequired)?;

    let mut analytics = AnalyticsReporter::only_error(
        layer_config.telemetry,
        ExecutionKind::Preview,
        watch,
        user_data.machine_id(),
        Some(key.to_owned()),
    );

    let (operator_api, _) = create_preview_api(
        &layer_config,
        true,
        &[NewOperatorFeature::PreviewWake],
        &progress,
        &mut analytics,
    )
    .await
    .inspect_err(|_| progress.failure(None))?;

    // Read through the operator's `previews` view, which every operator that wakes previews
    // serves, rather than the stored `PreviewSession`s: it carries each preview's wake timeout,
    // and marks the copies of multi-cluster previews whose primary runs elsewhere, which this
    // list asks for (see `local_previews_request`). The view filters by no label, so the key is
    // matched here, on the key the view reports.
    let (previews, copies) = under_key(
        operator_api
            .client()
            .request::<ObjectList<PreviewEnv>>(local_previews_request())
            .await
            .map_err(|error| CliError::PreviewListFailed(error.to_string()))
            .inspect_err(|_| progress.failure(None))?
            .items,
        key,
    );

    let previews = previews
        .into_iter()
        .filter(|preview| preview.metadata.name.is_some() && preview.metadata.namespace.is_some())
        .collect::<Vec<_>>();
    let wake_timeouts = wake_timeouts(
        previews
            .iter()
            .map(|preview| preview.spec.wake_timeout_secs),
        operator_api.operator().spec.preview_wake_timeout(),
        default_timeout,
    )
    .inspect_err(|_| progress.failure(None))?;

    let label = |namespace: &str, name: &str| match (several, context) {
        (true, Some(context)) => format!("{namespace}/{name} in context `{context}`"),
        (true, None) => format!("{namespace}/{name} in the default context"),
        (false, _) => format!("{namespace}/{name}"),
    };
    let copies = copies
        .into_iter()
        .map(|copy| PreviewCopy {
            label: label(
                copy.metadata.namespace.as_deref().unwrap_or_default(),
                copy.metadata.name.as_deref().unwrap_or_default(),
            ),
            primary_uid: copy.status.and_then(|status| status.primary_uid),
        })
        .collect();

    let operator_api = Arc::new(operator_api);
    let mut targets = Vec::new();
    let mut uids = Vec::new();
    for preview in previews {
        let (Some(name), Some(namespace)) = (preview.metadata.name, preview.metadata.namespace)
        else {
            continue;
        };
        let label = label(&namespace, &name);
        uids.extend(preview.metadata.uid);

        let operator_api = operator_api.clone();
        let connect: Connect<UpgradedConnection> = Box::new(move || {
            let operator_api = operator_api.clone();
            let namespace = namespace.clone();
            let name = name.clone();
            Box::pin(async move { operator_api.connect_preview_wake(&namespace, &name).await })
        });
        targets.push(WakeTarget { label, connect });
    }
    targets.sort_by(|a, b| a.label.cmp(&b.label));

    progress.success(Some(&match targets.len() {
        1 => "found 1 preview environment".to_owned(),
        found => format!("found {found} preview environments"),
    }));

    Ok(ScopePreviews {
        targets,
        wake_timeouts,
        uids,
        copies,
    })
}

/// How long each preview waits for its pods when it wakes, from its `wake_timeout_secs`, or
/// `operator_default` without one. The operator's default is needed only for a preview without a
/// timeout of its own when `default_timeout`, without `--timeout`, so the command fails for the
/// lack of it only then, and never when there is no preview to wake.
fn wake_timeouts(
    wake_timeout_secs: impl IntoIterator<Item = Option<u64>>,
    operator_default: Option<Duration>,
    default_timeout: bool,
) -> CliResult<Vec<Duration>> {
    wake_timeout_secs
        .into_iter()
        .map(|secs| match (secs, operator_default) {
            (Some(_), _) => Ok(PreviewIdleConfig::wake_timeout(secs, Duration::ZERO)),
            (None, Some(operator_default)) => {
                Ok(PreviewIdleConfig::wake_timeout(None, operator_default))
            }
            // `--timeout` replaces every preview's timeout, so this one is never read.
            (None, None) if default_timeout.not() => Ok(Duration::ZERO),
            (None, None) => Err(CliError::PreviewWakeTimeoutUnknown),
        })
        .collect()
}

/// The previews under `key` in `previews`: those of the context's own, and the copies that a
/// multi-cluster primary on another cluster fanned out to it. Only the primary's cluster wakes a
/// copy, through its fan-out, so the command names the copies instead of waking them.
fn under_key(previews: Vec<PreviewEnv>, key: &str) -> (Vec<PreviewEnv>, Vec<PreviewEnv>) {
    previews
        .into_iter()
        .filter(|preview| preview.spec.key == key)
        .partition(|preview| {
            preview
                .status
                .as_ref()
                .is_none_or(|status| status.copy.not())
        })
}

/// A warning for each of `copies` whose primary isn't among the previews that the run wakes,
/// `uids`. The run wakes the others through their primary's cluster, which reaches the default
/// cluster's copy and every replica copy with pods, the clusters of `PreviewReplicaClusters`; a
/// split-only copy has no pods to wake.
fn copy_warnings(copies: &[PreviewCopy], uids: &[String]) -> Vec<String> {
    copies
        .iter()
        .filter(|copy| {
            copy.primary_uid
                .as_ref()
                .is_none_or(|primary| uids.contains(primary).not())
        })
        .map(|copy| {
            format!(
                "not waking {}: it is a copy of a preview whose primary runs on another cluster; \
                 run `mirrord preview wake` in the context of the primary's cluster to wake it",
                copy.label
            )
        })
        .collect()
}

/// The list of every namespace's previews from the operator's `previews` view, with
/// `localOnly`, so that a multi-cluster primary answers from its own cluster without asking each
/// workload cluster for its copies' phases. The command needs none of them, and a cluster that
/// doesn't answer must not hold the wake of every preview: the wake itself reaches each cluster.
/// With `includeCopies`, the list also holds the copies that a primary on another cluster fanned
/// out to this one (see [`under_key`]).
fn local_previews_request() -> Request<Vec<u8>> {
    Request::get(format!(
        "{}?localOnly=true&includeCopies=true",
        PreviewEnv::url_path(&(), None)
    ))
    .body(Vec::new())
    .expect("a fixed path with a fixed query is a valid uri")
}

/// What the state of a preview on all its clusters means for the command.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Every cluster runs the preview, apart from replica clusters that can't be woken.
    Ready,

    /// A `mirrord exec` session pauses the preview on the cluster that runs it, so the command
    /// skips it. A paused replica copy is only a warning.
    Paused,

    /// The preview failed, for the reason given.
    Failed(String),

    /// The preview isn't ready yet. `triggered` says whether the operator asked it to wake on
    /// every cluster, which is all that `--no-wait` waits for. `unreachable` names each cluster
    /// that runs the preview itself and that the operator can't reach for now, or reconnects to,
    /// with why, which is why the preview isn't ready should the deadline pass.
    Waiting {
        triggered: bool,
        unreachable: Vec<String>,
    },
}

impl Verdict {
    /// What `status` means for the command, and a warning for each replica cluster that can't be
    /// woken, which the command reports whatever the verdict.
    ///
    /// `at_deadline` says whether the command stopped waiting: a replica cluster whose connection
    /// the operator re-establishes is waited for until then, and only a warning after.
    fn of(status: &WakeStatus, at_deadline: bool) -> (Self, Vec<String>) {
        let named = |cluster: &Option<String>, text: &str| match cluster {
            Some(cluster) => format!("cluster `{cluster}`: {text}"),
            None => text.to_owned(),
        };

        let mut warnings = Vec::new();
        let mut failures = Vec::new();
        let mut unreachable = Vec::new();
        let mut paused = false;
        let mut ready = status.clusters.is_empty().not();
        let mut triggered = ready;
        for wake in &status.clusters {
            match &wake.state {
                WakeState::Ready => {}
                WakeState::Unreachable { message } if wake.replica => warnings.push(named(
                    &wake.cluster,
                    &format!("can't reach the operator to wake the replica: {message}"),
                )),
                // The cluster runs the preview itself, so the preview isn't ready without it, and
                // the operator keeps trying to reach it.
                WakeState::Unreachable { message } => {
                    ready = false;
                    triggered = false;
                    unreachable.push(named(
                        &wake.cluster,
                        &format!("can't reach the operator to wake the preview: {message}"),
                    ));
                }
                WakeState::Reconnecting { message } if wake.replica && at_deadline => warnings
                    .push(named(
                        &wake.cluster,
                        &format!("{message}; reconnecting, the replica may stay as it is"),
                    )),
                WakeState::Reconnecting { message } => {
                    ready = false;
                    triggered = false;
                    if wake.replica.not() {
                        unreachable.push(named(&wake.cluster, &format!("{message}; reconnecting")));
                    }
                }
                WakeState::Unsupported => warnings.push(named(
                    &wake.cluster,
                    "the operator can't wake previews, so the replica stays as it is; upgrade \
                     the operator to wake it",
                )),
                WakeState::Pending => {
                    ready = false;
                    triggered = false;
                }
                WakeState::Failed { message } => failures.push(named(&wake.cluster, message)),
                // A `mirrord exec` session that pauses a replica copy leaves the preview itself,
                // and the replicas on the other clusters, to wake.
                WakeState::Paused if wake.replica => warnings.push(named(
                    &wake.cluster,
                    "a `mirrord exec` session pauses the replica, so it stays as it is until that \
                     session ends",
                )),
                WakeState::Paused => paused = true,
                WakeState::Waking | WakeState::Unknown => ready = false,
            }
        }

        let verdict = if failures.is_empty().not() {
            Self::Failed(failures.join("; "))
        } else if paused {
            Self::Paused
        } else if ready {
            Self::Ready
        } else {
            Self::Waiting {
                triggered,
                unreachable,
            }
        };
        (verdict, warnings)
    }
}

/// What the driver of one preview reports to the command.
#[derive(Debug)]
enum Report {
    Status(WakeStatus),

    /// The connection to the operator broke, and the driver connects again after the wait.
    Reconnecting {
        error: String,
        after: Duration,
    },

    /// The driver can't wake the preview, for a reason that no new connection changes.
    Fatal(String),
}

/// Keeps the wake websocket of preview `index` open, and reports each status that the operator
/// sends to `reports`, until the preview failed or is paused, which ends the wake, or the command
/// drops the driver. Opens the websocket again, with a growing wait, after it broke.
async fn drive<S: AsyncRead + AsyncWrite + Unpin>(
    index: usize,
    mut connect: Connect<S>,
    reports: mpsc::UnboundedSender<(usize, Report)>,
) {
    let mut delay = RECONNECT_DELAY;

    loop {
        let error = match connect().await {
            Ok(mut connection) => loop {
                match connection.next_status().await {
                    Ok(status) => {
                        delay = RECONNECT_DELAY;
                        let ends = matches!(
                            Verdict::of(&status, false).0,
                            Verdict::Failed(_) | Verdict::Paused
                        );
                        if reports.send((index, Report::Status(status))).is_err() || ends {
                            return;
                        }
                    }
                    Err(error @ PreviewWakeError::Decode(_)) => {
                        let _ = reports.send((index, Report::Fatal(with_causes(&error))));
                        return;
                    }
                    Err(error) => break with_causes(&error),
                }
            },
            Err(error) if refused(&error) => {
                let _ = reports.send((index, Report::Fatal(with_causes(&error))));
                return;
            }
            Err(error) => with_causes(&error),
        };

        let report = Report::Reconnecting {
            error,
            after: delay,
        };
        if reports.send((index, report)).is_err() {
            return;
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAX_RECONNECT_DELAY);
    }
}

/// One preview while the command waits for it.
struct Waiting {
    label: String,
    progress: ProgressTracker,
    done: bool,

    /// Why the preview's last status says that it isn't ready, when that is a cluster running
    /// the preview itself that the operator can't reach, or reconnects to (see
    /// [`Verdict::Waiting`]).
    unreachable: Vec<String>,

    /// The replica clusters that the preview's last status says can't be woken.
    warnings: Vec<String>,

    /// Why the connection to the operator last broke, until the operator sends a status again.
    disconnected: Option<String>,

    /// The last status the operator sent, which the deadline judges again (see [`Verdict::of`]).
    last: Option<WakeStatus>,
}

/// How [`wake_all`] ended.
#[derive(Debug, Default, PartialEq, Eq)]
struct WakeOutcome {
    /// Each preview that failed or wasn't ready in time, with why.
    failed: Vec<String>,

    /// Each replica cluster that can't be woken, after the preview it belongs to, whatever
    /// became of the preview.
    warnings: Vec<String>,
}

/// Wakes every preview of `targets` and waits until each one is ready, failed or paused, or
/// until `deadline`; with `no_wait`, only until the operator asked each one to wake.
///
/// Every connection stays open until this returns, so that no preview idles while the command
/// waits for the others.
async fn wake_all<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    targets: Vec<WakeTarget<S>>,
    deadline: Instant,
    no_wait: bool,
    progress: &ProgressTracker,
) -> WakeOutcome {
    let (reports_tx, mut reports_rx) = mpsc::unbounded_channel();
    let mut drivers = Vec::with_capacity(targets.len());
    let mut previews = Vec::with_capacity(targets.len());
    for (index, WakeTarget { label, connect }) in targets.into_iter().enumerate() {
        drivers.push(AbortOnDropHandle::new(tokio::spawn(drive(
            index,
            connect,
            reports_tx.clone(),
        ))));
        previews.push(Waiting {
            progress: progress.subtask(&format!("waking {label}")),
            label,
            done: false,
            unreachable: Vec::new(),
            warnings: Vec::new(),
            disconnected: None,
            last: None,
        });
    }
    drop(reports_tx);

    let mut failed = Vec::new();
    while previews.iter().any(|preview| preview.done.not()) {
        let report = tokio::select! {
            report = reports_rx.recv() => report,
            () = tokio::time::sleep_until(deadline) => break,
        };
        let Some((index, report)) = report else {
            break;
        };
        let Some(preview) = previews.get_mut(index).filter(|preview| preview.done.not()) else {
            continue;
        };

        match report {
            Report::Status(status) => {
                let (verdict, warnings) = Verdict::of(&status, false);
                preview.last = Some(status);
                preview.warnings = warnings;
                preview.disconnected = None;
                match verdict {
                    Verdict::Ready => {
                        preview.progress.success(Some("ready"));
                        preview.done = true;
                    }
                    Verdict::Paused => {
                        progress.warning(&format!(
                            "skipping {}: a `mirrord exec` session pauses it",
                            preview.label
                        ));
                        preview.progress.success(Some("skipped, paused"));
                        preview.done = true;
                    }
                    Verdict::Failed(message) => {
                        preview.progress.failure(Some(&message));
                        failed.push(format!("{} ({message})", preview.label));
                        preview.done = true;
                    }
                    Verdict::Waiting {
                        triggered: true, ..
                    } if no_wait => {
                        preview.progress.success(Some("waking"));
                        preview.done = true;
                    }
                    Verdict::Waiting { unreachable, .. } => preview.unreachable = unreachable,
                }
            }
            Report::Reconnecting { error, after } => {
                preview.progress.warning(&format!(
                    "{error}; reconnecting in {} seconds",
                    after.as_secs()
                ));
                preview.disconnected = Some(error);
            }
            Report::Fatal(message) => {
                preview.progress.failure(Some(&message));
                failed.push(format!("{} ({message})", preview.label));
                preview.done = true;
            }
        }
    }

    for preview in previews.iter_mut().filter(|preview| preview.done.not()) {
        // A status from before the command's own connection broke no longer holds: the hold went
        // with the connection.
        if let Some(status) = preview
            .last
            .as_ref()
            .filter(|_| preview.disconnected.is_none())
        {
            let (verdict, warnings) = Verdict::of(status, true);
            preview.warnings = warnings;
            match verdict {
                Verdict::Ready => {
                    preview.progress.success(Some("ready"));
                    continue;
                }
                Verdict::Waiting {
                    triggered: true, ..
                } if no_wait => {
                    preview.progress.success(Some("waking"));
                    continue;
                }
                Verdict::Waiting { unreachable, .. } => preview.unreachable = unreachable,
                Verdict::Paused | Verdict::Failed(_) => {}
            }
        }

        let late = if no_wait {
            "the operator didn't start waking it in time"
        } else {
            "not ready in time"
        };
        let unreachable = preview.unreachable.join("; ");
        let message = match (&preview.disconnected, unreachable.is_empty()) {
            (Some(error), false) => {
                format!("{unreachable}; then the connection to the operator broke: {error}")
            }
            (None, false) => unreachable,
            (Some(error), true) => format!("{late}, the connection to the operator broke: {error}"),
            (None, true) => late.to_owned(),
        };
        preview.progress.failure(Some(&message));
        failed.push(format!("{} ({message})", preview.label));
    }

    drop(drivers);
    WakeOutcome {
        failed,
        warnings: previews
            .into_iter()
            .flat_map(|preview| {
                let label = preview.label;
                preview
                    .warnings
                    .into_iter()
                    .map(move |warning| format!("{label}: {warning}"))
            })
            .collect(),
    }
}
