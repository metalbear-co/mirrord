use kube::{core::ErrorResponse, Api};
use mirrord_analytics::NullReporter;
use mirrord_config::LayerConfig;
use mirrord_operator::{
    client::{
        error::{OperatorApiError, OperatorOperation},
        MaybeClientCert, OperatorApi,
    },
    crd::SessionCrd,
};
use mirrord_progress::{Progress, ProgressTracker};

use crate::{CliError, Result, SessionCommand};

/// Handles the [`SessionCommand`]s that deal with session management in the operator.
pub(super) struct SessionCommandHandler {
    /// Final progress reporter, showing that the operation either succeeded or failed.
    progress: ProgressTracker,

    /// Progress reporter for the commands, we use this to give out details on how the
    /// operation is going.
    sub_progress: ProgressTracker,

    /// Api to talk with session routes in the operator.
    operator_api: OperatorApi<MaybeClientCert>,

    /// The command the user is trying to execute from the cli.
    command: SessionCommand,
}

impl SessionCommandHandler {
    /// Starts a new handler for [`SessionCommand`]s.
    #[tracing::instrument(level = "trace")]
    pub(super) async fn new(command: SessionCommand) -> Result<Self> {
        let mut progress = ProgressTracker::from_env("Operator session action");

        let config = LayerConfig::from_env().inspect_err(|error| {
            progress.failure(Some(&format!("failed to read config from env: {error}")));
        })?;

        let mut subtask = progress.subtask("checking operator");
        let operator_api = match OperatorApi::try_new(&config, &mut NullReporter::default()).await?
        {
            Some(api) => api.prepare_client_cert(&mut NullReporter::default()).await,
            None => {
                subtask.failure(Some("operator not found"));
                return Err(CliError::OperatorNotInstalled);
            }
        };

        operator_api.inspect_cert_error(|error| {
            progress.warning(&format!("Failed to prepare user certificate: {error}"));
        });

        let sub_progress = progress.subtask("preparing...");

        Ok(Self {
            progress,
            sub_progress,
            operator_api,
            command,
        })
    }

    /// Does the actual work of talking to the operator through the kube [`Api`], using
    /// the routes defined in [`SessionCrd`].
    #[tracing::instrument(level = "trace", skip(self), ret)]
    pub(super) async fn handle(self) -> Result<()> {
        let Self {
            mut progress,
            mut sub_progress,
            operator_api,
            command,
        } = self;

        sub_progress.print(&format!("executing `{command}`"));

        let session_api: Api<SessionCrd> = Api::all(operator_api.client().clone());

        // We're interested in the `Status`es, so we map the results into those.
        match command {
            SessionCommand::Kill { id } => session_api
                .delete(&format!("{id}"), &Default::default())
                .await
                .map(|either| either.right()),
            SessionCommand::KillAll => session_api
                .delete_collection(&Default::default(), &Default::default())
                .await
                .map(|either| either.right()),
            SessionCommand::RetainActive => session_api
                .delete("inactive", &Default::default())
                .await
                .map(|either| either.right()),
        }
        .map_err(|kube_fail| match kube_fail {
            // The random `reason` we get when the operator returns from a "missing route".
            kube::Error::Api(ErrorResponse { code, reason, .. })
                if code == 404 && reason.contains("parse") =>
            {
                OperatorApiError::UnsupportedFeature {
                    feature: "session management".to_string(),
                    operator_version: operator_api.operator().spec.operator_version.clone(),
                }
            }
            // Something actually went wrong.
            other => OperatorApiError::KubeError {
                error: other,
                operation: OperatorOperation::SessionManagement,
            },
        })
        // Finish the progress report here if we have an error response. 
        .inspect_err(|fail| {
            sub_progress.failure(Some(&fail.to_string()));
            progress.failure(Some("Session management operation failed!"));
        })?
        // The kube api interaction was successful, but we might still fail the operation
        // itself, so let's check the `Status` and report.
        .map(|status| {
            if status.is_failure() {
                sub_progress.failure(Some(&format!(
                    "`{command}` failed with code {}: {}",
                    status.code, status.message
                )));
                progress.failure(Some("Session operation failed!"));

                Err(OperatorApiError::StatusFailure {
                    operation: OperatorOperation::SessionManagement,
                    status: Box::new(status),
                })
            } else {
                sub_progress.success(Some(&format!(
                    "`{command}` finished successfully with code {}: {}",
                    status.code, status.message
                )));
                progress.success(Some("Session operation is completed."));

                Ok(())
            }
        })
        .transpose()?
        // We might've gotten a `SessionCrd` instead of a `Status` (we have a `Left(T)`),
        // meaning that the operation has started, but it might not be finished yet.
        .unwrap_or_else(|| {
            sub_progress.success(Some(&format!("No issues found when executing `{command}`, but the operation status could not be determined at this time.")));
            progress.success(Some(&format!("`{command}` is done, but the operation might be pending.")));
        });

        Ok(())
    }
}
