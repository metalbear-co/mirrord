use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use humansize::{format_size, DECIMAL};
use k8s_openapi::api::core::v1::ConfigMap;
use mirrord_analytics::{AnalyticsError, NullReporter, Reporter};
use mirrord_config::{agent::AgentImageConfig, LayerConfig};
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_protocol::{
    vpn::{ClientVpn, ServerVpn},
    ClientMessage, DaemonMessage,
};
use mirrord_vpn::config::VpnConfig;
use tokio::signal;

use crate::{
    config::VpnArgs,
    connection::{create_and_connect, AgentConnection},
    error::{CliError, Result},
};

async fn agent_request<T>(
    connection: &mut AgentConnection,
    request: ClientMessage,
    mapper: impl Fn(DaemonMessage) -> Option<T>,
) -> Option<T> {
    tracing::debug!(client_message = ?request, "out");
    connection.sender.send(request).await.ok()?;

    loop {
        let daemon_message = connection.receiver.recv().await?;

        tracing::debug!(?daemon_message, "in");

        let extracted = mapper(daemon_message);

        if extracted.is_some() {
            break extracted;
        }
    }
}

fn get_server_vpn(message: DaemonMessage) -> Option<ServerVpn> {
    match message {
        DaemonMessage::Vpn(response) => Some(response),
        _ => None,
    }
}

#[allow(clippy::indexing_slicing)]
pub async fn vpn_command(args: VpnArgs) -> Result<()> {
    let mut progress = ProgressTracker::from_env("mirrord vpn");
    let mut sub_progress = progress.subtask("create agent");

    let mut analytics = NullReporter::default();

    let mut config = LayerConfig::from_env()?;

    config.agent.image = AgentImageConfig("test".into());
    config.agent.privileged = true;
    config.operator = Some(false);
    config.target.path = None;
    config.target.namespace = args.namespace;

    let client = kube::Client::try_default()
        .await
        .map_err(|err| CliError::CreateKubeApiFailed(err.into()))?;

    let configmap_api = kube::Api::<ConfigMap>::namespaced(client, "kube-system");

    let Some(vpn_config) = VpnConfig::from_configmaps(&configmap_api).await else {
        return Ok(());
    };

    let (_, mut connection) = create_and_connect(&config, &mut sub_progress, &mut analytics)
        .await
        .inspect_err(|_| analytics.set_error(AnalyticsError::AgentConnection))?;

    sub_progress.success(None);
    progress.success(None);

    let Some(ServerVpn::NetworkConfiguration(network)) = agent_request(
        &mut connection,
        ClientMessage::Vpn(ClientVpn::GetNetworkConfiguration),
        get_server_vpn,
    )
    .await
    else {
        return Ok(());
    };

    tracing::debug!(?network, "loaded vpn network configuration");

    let (mut write_stream, read_stream) = mirrord_vpn::socket::create_vpn_socket(&network).split();
    let read_stream = read_stream.fuse();
    tokio::pin!(read_stream);

    #[cfg(not(target_os = "macos"))]
    let linux_guard = {
        use mirrord_vpn::linux::*;

        let Ok(resolv_override) = ResolvOverride::accuire_override("/etc/resolv.conf")
            .await
            .inspect_err(|error| tracing::info!(%error, "unabable to override /etc/resolv.conf"))
        else {
            return Ok(());
        };

        if resolv_override
            .update_resolv(&vpn_config.dns_nameservers)
            .await
            .is_err()
        {
            let _ = resolv_override.unmount().await;

            return Ok(());
        }

        resolv_override
    };

    #[cfg(target_os = "macos")]
    let macos_guard = {
        use mirrord_vpn::macos::*;

        let subnet_guard = create_subnet_route(&vpn_config.service_subnet, &network.gateway)
            .await
            .map_err(CliError::RuntimeError)?;

        let resolve_guard = ResolveFile {
            port: 53,
            domain: vpn_config.dns_domain,
            nameservers: vpn_config.dns_nameservers,
            ..Default::default()
        }
        .inject()
        .await
        .map_err(CliError::RuntimeError)?;

        (subnet_guard, resolve_guard)
    };

    let packets_sent = AtomicUsize::default();
    let bytes_sent = AtomicUsize::default();

    let packets_recived = AtomicUsize::default();
    let bytes_recived = AtomicUsize::default();

    let mut statistic_interval = tokio::time::interval(Duration::from_secs(10));

    'main: loop {
        tokio::select! {
            packet = read_stream.next() => {
                let packet = packet.unwrap().unwrap();
                let packet_length = packet.len();
                if connection
                    .sender
                    .send(mirrord_protocol::ClientMessage::Vpn(ClientVpn::Packet(
                        packet,
                    )))
                    .await
                    .is_err()
                {
                    break 'main;
                }

                bytes_sent.fetch_add(packet_length, Ordering::Relaxed);
                packets_sent.fetch_add(1, Ordering::Relaxed);
            }
            message = connection.receiver.recv() => {
                if let Some(message) = message {
                    match message {
                        DaemonMessage::Vpn(ServerVpn::Packet(packet)) => {
                            let packet_length = packet.len();
                            if let Err(err) = write_stream.send(packet).await {
                                tracing::warn!(%err, "Unable to pipe back packet")
                            }

                            bytes_recived.fetch_add(packet_length, Ordering::Relaxed);
                            packets_recived.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => unimplemented!("Unexpected response from agent"),
                    }
                } else {
                    break 'main;
                }
            }

            _ = statistic_interval.tick() => {
                tracing::debug!(
                    bytes_sent = %format_size(bytes_sent.load(Ordering::Relaxed), DECIMAL),
                    packets_sent = %packets_sent.load(Ordering::Relaxed),
                    bytes_recived = %format_size(bytes_recived.load(Ordering::Relaxed), DECIMAL),
                    packets_recived = %packets_recived.load(Ordering::Relaxed),
                    "stats"
                );
            }

            _ = signal::ctrl_c() => {
                break 'main;
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = linux_guard.unmount().await;
    }

    #[cfg(target_os = "macos")]
    {
        let (subnet_guard, resolve_guard) = macos_guard;

        let _ = subnet_guard.unmount().await;
        let _ = resolve_guard.unmount().await;
    }

    Ok(())
}
