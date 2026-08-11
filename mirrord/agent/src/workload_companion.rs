mod handoff;
mod incoming;

use mirrord_agent_env::envs;
use tokio_util::sync::CancellationToken;

use self::{
    handoff::ConnectionHandoffServer,
    incoming::{RemoteLayerIncoming, RemoteLayerPortRedirector},
};
use crate::{
    error::AgentResult,
    incoming::{
        MirrorHandle, RedirectorTask, RedirectorTaskConfig, StealHandle, tls::StealTlsHandlerStore,
    },
    task::{
        BgTaskRuntime,
        status::{BgTaskStatus, IntoStatus},
    },
    util::path_resolver::InTargetPathResolver,
};

/// Starts and monitors the workload-companion's remote-layer incoming traffic infrastructure.
///
/// The returned handles connect client steal and mirror requests to the shared redirector, while
/// the private status tracks the redirector and connection-handoff server running on the target's
/// network runtime.
#[derive(Debug)]
pub(crate) struct WorkloadCompanionIngress {
    pub(crate) steal_handle: StealHandle,
    pub(crate) mirror_handle: MirrorHandle,
    status: BgTaskStatus,
}

impl WorkloadCompanionIngress {
    pub(crate) async fn start(
        runtime: &BgTaskRuntime,
        cancellation_token: CancellationToken,
    ) -> AgentResult<Self> {
        let tls_steal_config = envs::STEAL_TLS_CONFIG.from_env_or_default();
        let target_pid = runtime.target_pid().unwrap_or(1);
        let tls_handler_store =
            StealTlsHandlerStore::new(tls_steal_config, InTargetPathResolver::new(target_pid));
        let redirector_task_config = RedirectorTaskConfig::from_env();
        let RemoteLayerIncoming {
            redirector,
            sender,
            subscriptions,
        } = RemoteLayerIncoming::new();
        let (redirector_task, steal_handle, mirror_handle) =
            RedirectorTask::new(redirector, tls_handler_store, redirector_task_config);
        let handoff_server = ConnectionHandoffServer::bind(sender, subscriptions)?;

        let status = runtime
            .handle()
            .spawn(run_ingress(
                redirector_task,
                handoff_server,
                cancellation_token,
            ))
            .into_status("WorkloadCompanionIngress");

        Ok(Self {
            steal_handle,
            mirror_handle,
            status,
        })
    }

    pub(crate) fn status(&self) -> BgTaskStatus {
        self.status.clone()
    }
}

async fn run_ingress(
    redirector_task: RedirectorTask<RemoteLayerPortRedirector>,
    handoff_server: ConnectionHandoffServer,
    cancellation_token: CancellationToken,
) {
    tokio::select! {
        // The redirector is required for delivering accepted handoffs to steal and mirror clients,
        // so any termination ends the shared ingress background task.
        result = redirector_task.run() => {
            if let Err(error) = result {
                tracing::error!(%error, "remote-layer ingress task failed");
            } else {
                tracing::warn!("remote-layer ingress task stopped unexpectedly");
            }
        }
        // The handoff server is the remote layer's only ingress path, so any failure or unexpected
        // termination also ends the shared ingress background task.
        result = handoff_server.run(cancellation_token.clone()) => {
            if let Err(error) = result {
                tracing::error!(%error, "connection handoff server failed");
            } else if !cancellation_token.is_cancelled() {
                tracing::warn!("connection handoff server stopped unexpectedly");
            }
        }
        // Normal workload-companion shutdown drops both ingress futures and lets their owned
        // resources clean themselves up.
        _ = cancellation_token.cancelled() => {}
    }
}
