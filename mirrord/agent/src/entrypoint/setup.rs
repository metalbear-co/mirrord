use mirrord_agent_env::envs;

use crate::{
    error::{AgentError, AgentResult},
    incoming::{self, RedirectorTask, StealHandle},
    util::remote_runtime::RemoteRuntime,
};

/// Starts a [`RedirectorTask`] on the given `runtime`.
///
/// Returns the [`StealHandle`] that can be used to steal incoming traffic.
pub(super) async fn start_traffic_redirector(runtime: &RemoteRuntime) -> AgentResult<StealHandle> {
    let flush_connections = envs::STEALER_FLUSH_CONNECTIONS.from_env_or_default();
    let pod_ips = envs::POD_IPS.from_env_or_default();
    let support_ipv6 = envs::IPV6_SUPPORT.from_env_or_default();

    let (task, handle) = runtime
        .spawn(async move {
            incoming::create_iptables_redirector(flush_connections, &pod_ips, support_ipv6)
                .await
                .map(RedirectorTask::new)
        })
        .await
        .map_err(|error| AgentError::IPTablesSetupError(error.into()))?
        .map_err(|error| AgentError::IPTablesSetupError(error.into()))?;

    runtime.spawn(task.run());

    Ok(handle)
}
