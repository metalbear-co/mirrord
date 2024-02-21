use kube::{api::DeleteParams, core::Status, Api};
use mirrord_operator::client::{session_api, OperatorApiError, OperatorOperation};
use mirrord_progress::{Progress, ProgressTracker};
use tracing::error;

use crate::{error::CliError, Result};

/// Prepares progress and kube api for use in the operator session commands.
#[tracing::instrument(level = "trace", ret)]
async fn operator_session_prepare() -> Result<(
    ProgressTracker,
    Api<mirrord_operator::crd::SessionCrd>,
    ProgressTracker,
)> {
    let progress = ProgressTracker::from_env("Operator session action");

    let session_api = session_api(None)
        .await
        .inspect_err(|fail| error!(error = ?fail, "Failed to create session API"))?;

    let sub_progress = progress.subtask("preparing...");

    Ok((progress, session_api, sub_progress))
}

/// Handles the cleanup part of progress after an operator session command.
#[tracing::instrument(level = "trace")]
fn operator_session_finished(
    result: Option<Status>,
    mut sub_progress: ProgressTracker,
    mut progress: ProgressTracker,
) {
    match result {
        Some(status) => {
            if status.is_failure() {
                sub_progress.failure(Some(&format!(
                    "session operation failed with {}!",
                    status.code
                )));
                progress.failure(Some("Session operation failed!"));
            } else {
                sub_progress.success(Some(&format!(
                    "session operation finished successfully with {}!",
                    status.code
                )));
                progress.success(Some("Session operation is completed!"));
            }
        }
        None => {
            sub_progress.print("session operation pending");
            progress.print("Operation is pending.");
        }
    }
}

/// `mirrord operator session kill_all`: kills every operator session, this is basically a
/// `.clear()`;
#[tracing::instrument(level = "trace", ret)]
pub(super) async fn operator_session_kill_all() -> Result<()> {
    let (progress, api, sub_progress) = operator_session_prepare().await?;

    sub_progress.print("killing all sessions");

    let result = api
        .delete("active", &DeleteParams::default())
        .await
        .map_err(|error| OperatorApiError::KubeError {
            error,
            operation: OperatorOperation::GettingStatus,
        })
        .map_err(CliError::from)?;

    operator_session_finished(result.right(), sub_progress, progress);

    Ok(())
}

/// `mirrord operator session kill --id {id}`: kills the operator session specified by `id`.
#[tracing::instrument(level = "trace", ret)]
pub(super) async fn operator_session_kill_one(id: u64) -> Result<()> {
    let (progress, api, sub_progress) = operator_session_prepare().await?;

    sub_progress.print("killing session with id {session_id}");

    let result = api
        .delete(&format!("active/{id}"), &DeleteParams::default())
        .await
        .map_err(|error| OperatorApiError::KubeError {
            error,
            operation: OperatorOperation::GettingStatus,
        })
        .map_err(CliError::from)?;

    operator_session_finished(result.right(), sub_progress, progress);

    Ok(())
}

/// `mirrord operator session kill {id}`: performs a clean-up for operator sessions that are still
/// stored;
#[tracing::instrument(level = "trace", ret)]
pub(super) async fn operator_session_retain_active() -> Result<()> {
    let (progress, api, sub_progress) = operator_session_prepare().await?;

    sub_progress.print("retaining only active sessions");

    let result = api
        .delete("inactive", &DeleteParams::default())
        .await
        .map_err(|error| OperatorApiError::KubeError {
            error,
            operation: OperatorOperation::GettingStatus,
        })
        .map_err(CliError::from)?;

    operator_session_finished(result.right(), sub_progress, progress);

    Ok(())
}
