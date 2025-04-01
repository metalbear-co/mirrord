use mirrord_agent_env::envs;
use tokio::sync::oneshot;

use crate::{
    error::{AgentError, AgentResult},
    incoming::{self, RedirectorTask, StealHandle},
    util::run_thread_in_namespace,
};

/// Starts a [`RedirectorTask`] in the target's network namespace.
///
/// Returns the [`StealHandle`] that can be used to steal incoming traffic.
pub(crate) async fn start_traffic_redirector(target_pid: u64) -> AgentResult<StealHandle> {
    let flush_connections = envs::STEALER_FLUSH_CONNECTIONS.from_env_or_default();
    let pod_ips = envs::POD_IPS.from_env_or_default();
    let support_ipv6 = envs::IPV6_SUPPORT.from_env_or_default();

    let (handle_tx, handle_rx) = oneshot::channel();

    run_thread_in_namespace(
        async move {
            let redirector_result =
                incoming::create_iptables_redirector(flush_connections, &pod_ips, support_ipv6)
                    .await;

            let redirector = match redirector_result {
                Ok(redirector) => redirector,
                Err(error) => {
                    let _ = handle_tx.send(Err(error));
                    return;
                }
            };

            let (task, handle) = RedirectorTask::new(redirector);

            if handle_tx.send(Ok(handle)).is_err() {
                return;
            }

            let _ = task.run().await.inspect_err(|error| {
                tracing::error!(%error, "Incoming traffic redirector task failed");
            });
        },
        "IncomingTrafficRedirector".into(),
        Some(target_pid),
        "net",
    );

    match handle_rx.await {
        Ok(result) => result.map_err(|error| AgentError::IPTablesSetupError(error.into())),
        Err(..) => Err(AgentError::IPTablesSetupError("task panicked".into())),
    }
}
