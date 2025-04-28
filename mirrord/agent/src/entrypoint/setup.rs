use mirrord_agent_env::envs;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::BackgroundTask;
use crate::{
    dns::{DnsCommand, DnsWorker},
    error::{AgentError, AgentResult},
    incoming::{self, tls::IncomingTlsHandlerStore, MirrorHandle, RedirectorTask, StealHandle},
    sniffer::{messages::SnifferCommand, TcpConnectionSniffer},
    steal::{StealerCommand, TcpStealerTask},
    util::{
        path_resolver::InTargetPathResolver,
        remote_runtime::{BgTaskRuntime, IntoStatus},
    },
};

/// Starts a [`RedirectorTask`] on the given `runtime`.
///
/// Returns the [`StealHandle`] that can be used to steal incoming traffic.
pub(super) async fn start_traffic_redirector(
    runtime: &BgTaskRuntime,
    target_pid: u64,
) -> AgentResult<(StealHandle, MirrorHandle)> {
    let flush_connections = envs::STEALER_FLUSH_CONNECTIONS.from_env_or_default();
    let pod_ips = envs::POD_IPS.from_env_or_default();
    let support_ipv6 = envs::IPV6_SUPPORT.from_env_or_default();
    let tls_steal_config = envs::STEAL_TLS_CONFIG.from_env_or_default();

    let tls_handlers =
        IncomingTlsHandlerStore::new(tls_steal_config, InTargetPathResolver::new(target_pid));

    let (task, steal_handle, mirror_handle) = runtime
        .spawn(async move {
            incoming::create_iptables_redirector(flush_connections, &pod_ips, support_ipv6)
                .await
                .map(|redirector| RedirectorTask::new(redirector, tls_handlers))
        })
        .await
        .map_err(|error| AgentError::IPTablesSetupError(error.into()))?
        .map_err(|error| AgentError::IPTablesSetupError(error.into()))?;

    runtime.spawn(task.run());

    Ok((steal_handle, mirror_handle))
}

pub(super) async fn start_sniffer(
    args: &super::Args,
    runtime: &BgTaskRuntime,
    cancellation_token: CancellationToken,
) -> BackgroundTask<SnifferCommand> {
    let (command_tx, command_rx) = mpsc::channel::<SnifferCommand>(1000);

    let sniffer = runtime
        .spawn(TcpConnectionSniffer::new(
            command_rx,
            args.network_interface.clone(),
            args.is_mesh(),
        ))
        .await;

    match sniffer {
        Ok(Ok(sniffer)) => {
            let task_status = runtime
                .spawn(sniffer.start(cancellation_token.clone()))
                .into_status("TcpSnifferTask");

            BackgroundTask::Running(task_status, command_tx)
        }
        Ok(Err(error)) => {
            tracing::error!(%error, "Failed to create a TCP sniffer");
            BackgroundTask::Disabled
        }
        Err(error) => {
            tracing::error!(%error, "Failed to create a TCP sniffer");
            BackgroundTask::Disabled
        }
    }
}

pub(super) fn start_stealer(
    runtime: &BgTaskRuntime,
    steal_handle: StealHandle,
) -> BackgroundTask<StealerCommand> {
    let (command_tx, command_rx) = mpsc::channel::<StealerCommand>(1000);

    let task_status = runtime
        .spawn(TcpStealerTask::new(steal_handle, command_rx).run())
        .into_status("TcpStealerTask");

    BackgroundTask::Running(task_status, command_tx)
}

pub(super) fn start_dns(
    args: &super::Args,
    runtime: &BgTaskRuntime,
    cancellation_token: CancellationToken,
) -> BackgroundTask<DnsCommand> {
    let (command_tx, command_rx) = mpsc::channel::<DnsCommand>(1000);

    let task_status = runtime
        .spawn(
            DnsWorker::new(runtime.target_pid(), command_rx, args.ipv6)
                .run(cancellation_token.clone()),
        )
        .into_status("DnsTask");

    BackgroundTask::Running(task_status, command_tx)
}
