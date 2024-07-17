use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use humansize::{format_size, DECIMAL};
use k8s_openapi::api::core::v1::ConfigMap;
use mirrord_analytics::{AnalyticsError, NullReporter, Reporter};
use mirrord_config::LayerConfig;
use mirrord_kube::api::kubernetes::create_kube_config;
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

    connection.receiver.recv().await.and_then(mapper)
}

#[cfg(not(target_os = "macos"))]
async fn agent_fetch_file<const B: u64>(
    connection: &mut AgentConnection,
    path: std::path::PathBuf,
) -> mirrord_protocol::RemoteResult<Vec<u8>> {
    use mirrord_protocol::{file::*, FileRequest, FileResponse};

    let request = FileRequest::Open(OpenFileRequest {
        path,
        open_options: OpenOptionsInternal {
            read: true,
            ..Default::default()
        },
    });

    let Some(FileResponse::Open(response)) = agent_request(
        connection,
        ClientMessage::FileRequest(request),
        get_file_response,
    )
    .await
    else {
        todo!()
    };

    let OpenFileResponse { fd } = response?;

    let request = FileRequest::Read(ReadFileRequest {
        remote_fd: fd,
        buffer_size: B,
    });

    let Some(FileResponse::Read(response)) = agent_request(
        connection,
        ClientMessage::FileRequest(request),
        get_file_response,
    )
    .await
    else {
        todo!();
    };

    let ReadFileResponse { bytes, .. } = response?;

    let request = FileRequest::Close(CloseFileRequest { fd });

    let _ = connection
        .sender
        .send(ClientMessage::FileRequest(request))
        .await;

    Ok(bytes)
}

fn get_server_vpn(message: DaemonMessage) -> Option<ServerVpn> {
    match message {
        DaemonMessage::Vpn(response) => Some(response),
        _ => None,
    }
}

#[cfg(not(target_os = "macos"))]
fn get_file_response(message: DaemonMessage) -> Option<mirrord_protocol::FileResponse> {
    match message {
        DaemonMessage::File(response) => Some(response),
        _ => None,
    }
}

#[allow(clippy::indexing_slicing)]
pub async fn vpn_command(args: VpnArgs) -> Result<()> {
    let mut progress = ProgressTracker::from_env("mirrord vpn");
    let mut sub_progress = progress.subtask("create agent");

    let mut analytics = NullReporter::default();

    let mut config = LayerConfig::from_env()?;
    config.agent.privileged = true;
    config.target.path = None;
    config.target.namespace = args.namespace;

    let client = create_kube_config(
        config.accept_invalid_certificates,
        config.kubeconfig.clone(),
        config.kube_context.clone(),
    )
    .await
    .and_then(|config| kube::Client::try_from(config).map_err(From::from))
    .map_err(CliError::CreateKubeApiFailed)?;

    let configmap_api = kube::Api::<ConfigMap>::namespaced(client, "kube-system");

    // TODO: this may fail but
    let Some(vpn_config) = VpnConfig::from_configmaps(&configmap_api).await else {
        return Ok(());
    };

    let (_, mut connection) = create_and_connect(&config, &mut sub_progress, &mut analytics)
        .await
        .inspect_err(|_| analytics.set_error(AnalyticsError::AgentConnection))?;

    sub_progress.success(None);

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

    let mut sub_progress = progress.subtask("create tun socket");

    let (mut write_stream, read_stream) = mirrord_vpn::socket::create_vpn_socket(&network).split();
    let read_stream = read_stream.fuse();
    tokio::pin!(read_stream);

    sub_progress.success(None);

    #[cfg(not(target_os = "macos"))]
    let linux_guard = {
        use mirrord_vpn::linux::*;

        let Ok(remote_resolv) =
            agent_fetch_file::<10000>(&mut connection, "/etc/resolv.conf".into()).await
        else {
            return Ok(());
        };

        let Ok(resolv_override) = ResolvOverride::accuire_override("/etc/resolv.conf")
            .await
            .inspect_err(|error| tracing::info!(%error, "unabable to override /etc/resolv.conf"))
        else {
            return Ok(());
        };

        if resolv_override.update_resolv(&remote_resolv).await.is_err() {
            let _ = resolv_override.unmount().await;

            return Ok(());
        }

        let _ = tokio::process::Command::new("ip")
            .args([
                "route".to_owned(),
                "add".to_owned(),
                vpn_config.service_subnet.to_string(),
                "via".to_owned(),
                network.gateway.to_string(),
            ])
            .output()
            .await
            .inspect_err(|error| tracing::error!(%error, "could not bind service_subnet"));

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

    progress.success(None);

    let _ = connection
        .sender
        .send(ClientMessage::Vpn(ClientVpn::OpenSocket))
        .await;

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
