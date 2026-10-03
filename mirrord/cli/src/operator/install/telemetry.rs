//! Usage reporting for `mirrord operator install` and `mirrord operator uninstall`: one event for
//! each run, with its outcome.
//!
//! [`AnalyticValue`](mirrord_analytics::AnalyticValue) has no string variant, so the outcome and
//! the phase that a run ended in are sent as numbers from the `#[repr(u32)]` enums below. Nothing
//! that identifies the cluster, the license or the user is sent: no API key, claim URL, cluster
//! ID, kubecontext, namespace or manifest content.

use mirrord_analytics::{Analytics, AnalyticsReporter, ReportTarget};
use uuid::Uuid;

use super::OperatorInstallError;

#[cfg(test)]
mod tests;

/// What the commands need to report their runs. Built by the CLI, which owns the machine id, the
/// telemetry setting and the drain that sends pending reports before the CLI exits.
pub(crate) struct OperatorTelemetry {
    /// `false` when the user turned telemetry off; nothing is sent then.
    pub(crate) enabled: bool,
    pub(crate) machine_id: Uuid,
    pub(crate) watch: drain::Watch,
}

impl OperatorTelemetry {
    /// Starts the report of a run. It is sent when the returned reporter is dropped, with the time
    /// until then as the duration of the run.
    pub(super) fn reporter(&self, target: ReportTarget) -> AnalyticsReporter {
        AnalyticsReporter::for_event(target, self.enabled, self.watch.clone(), self.machine_id)
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub(super) enum Outcome {
    Success = 0,
    Failure = 1,
    /// The user answered no to the confirmation prompt.
    Declined = 2,
    /// `mirrord operator uninstall` found no operator to remove.
    NotInstalled = 3,
    /// The user stopped the command, e.g. with Ctrl+C.
    Interrupted = 4,
}

/// The step of `mirrord operator install` that a run is in, reported when the run does not
/// complete.
#[derive(Debug, Clone, Copy, Default)]
#[repr(u32)]
pub(super) enum InstallPhase {
    /// Loading the kubeconfig and creating the clients.
    #[default]
    Connect = 1,
    CheckExistingOperator = 2,
    FetchManifest = 3,
    /// Checking that the objects can be created, and that no other objects are in the way.
    CheckPermissions = 4,
    Confirm = 5,
    StartTrial = 6,
    CreateObjects = 7,
    WaitForOperator = 8,
}

/// The step of `mirrord operator uninstall` that a run is in, reported when the run does not
/// complete.
#[derive(Debug, Clone, Copy, Default)]
#[repr(u32)]
pub(super) enum UninstallPhase {
    /// Loading the kubeconfig and creating the clients.
    #[default]
    Connect = 1,
    FetchManifest = 2,
    /// Finding the installed objects, and checking that `mirrord operator install` made them.
    FindInstallation = 3,
    Confirm = 4,
    FinalizeSessions = 5,
    DeleteObjects = 6,
    WaitForRemoval = 7,
}

/// What a run of `mirrord operator install` did, filled in as it goes.
#[derive(Debug, Default)]
pub(super) struct InstallRun {
    pub(super) phase: InstallPhase,
    /// Set when a trial started: how to retry without starting another trial, printed for the
    /// user when the run does not complete.
    pub(super) retry: Option<String>,
}

/// Adds the `outcome` of a run, and the `phase` that it ended in if it failed or was interrupted.
///
/// `result` is the outcome of a run that completed, or the error that ended it.
pub(super) fn add_outcome(
    analytics: &mut Analytics,
    result: Result<Outcome, &OperatorInstallError>,
    phase: u32,
) {
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(OperatorInstallError::Declined) => Outcome::Declined,
        Err(OperatorInstallError::Interrupted) => Outcome::Interrupted,
        Err(_) => Outcome::Failure,
    };

    analytics.add("outcome", outcome as u32);
    if let Outcome::Failure | Outcome::Interrupted = outcome {
        analytics.add("phase", phase);
    }
}
