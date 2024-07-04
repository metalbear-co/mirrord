use futures::{SinkExt, StreamExt};
use mirrord_analytics::{AnalyticsError, NullReporter, Reporter};
use mirrord_config::{agent::AgentImageConfig, LayerConfig};
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_protocol::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    vpn::{ClientVpn, ServerVpn},
    ClientMessage, DaemonMessage,
};
use tokio::signal;

use crate::{
    config::VpnArgs,
    connection::{create_and_connect, AgentConnection},
    error::Result,
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

fn get_address_info(message: DaemonMessage) -> Option<GetAddrInfoResponse> {
    match message {
        DaemonMessage::GetAddrInfoResponse(response) => Some(response),
        _ => None,
    }
}

pub async fn vpn_command(args: VpnArgs) -> Result<()> {
    let mut progress = ProgressTracker::from_env("mirrord vpn");
    let mut sub_progress = progress.subtask("create agent");

    let mut analytics = NullReporter::default();

    let mut config = LayerConfig::from_env()?;
    config.agent.image = AgentImageConfig("test".into());
    config.agent.log_level = "warn,mirrord=trace".to_string();
    config.agent.privileged = true;
    config.agent.ttl = 30;

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

    let Some(GetAddrInfoResponse(Ok(dns))) = agent_request(
        &mut connection,
        ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest {
            node: format!("kube-dns.kube-system.svc.{}", args.cluster_domain),
        }),
        get_address_info,
    )
    .await
    else {
        return Ok(());
    };

    tracing::debug!(?dns, "loaded vpn dns configuration");

    let (mut write_stream, read_stream) = mirrord_vpn::socket::create_vpn_socket(&network).split();
    let read_stream = read_stream.fuse();
    tokio::pin!(read_stream);

    #[cfg(target_os = "macos")]
    let macos_guard = {
        use tokio::process::Command;

        if let Some(subnet) = args.service_subnet.as_ref() {
            let output = Command::new("route")
                .args([
                    "-n",
                    "add",
                    "-net",
                    &subnet.to_string(),
                    &network.gateway.to_string(),
                ])
                .output()
                .await;

            println!("{output:?}");
        }

        mirrord_vpn::macos::ResolveFile {
            port: 53,
            domain: args.cluster_domain.clone(),
            nameservers: dns.iter().map(|resolved| resolved.ip.to_string()).collect(),
            ..Default::default()
        }
        .inject()
        .await
        .expect("Unable to inject mirrord resolve override")
    };

    'main: loop {
        tokio::select! {
            packet = read_stream.next() => {
                let packet = packet.unwrap().unwrap();
                connection
                    .sender
                    .send(mirrord_protocol::ClientMessage::Vpn(ClientVpn::Packet(packet)))
                    .await
                    .unwrap();
            }
            message = connection.receiver.recv() => {
                if let Some(message) = message {
                    match message {
                        DaemonMessage::Vpn(ServerVpn::Packet(packet)) => {
                            if let Err(err) = write_stream.send(packet).await {
                                tracing::warn!(%err, "Unable to pipe back packet")
                            }
                        }
                        _ => unimplemented!("Unexpected response from agent"),
                    }
                }
            }
            _ = signal::ctrl_c() => {
                break 'main;
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        use tokio::process::Command;

        if let Some(subnet) = args.service_subnet.as_ref() {
            let output = Command::new("route")
                .args([
                    "-n",
                    "delete",
                    "-net",
                    &subnet.to_string(),
                    &network.gateway.to_string(),
                ])
                .output()
                .await;

            println!("{output:?}");
        }

        let _ = macos_guard.cleanup().await;
    }

    Ok(())
}
