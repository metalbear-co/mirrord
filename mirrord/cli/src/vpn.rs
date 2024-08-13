use k8s_openapi::api::core::v1::ConfigMap;
use mirrord_analytics::{AnalyticsError, NullReporter, Reporter};
use mirrord_config::LayerConfig;
use mirrord_kube::api::kubernetes::create_kube_config;
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_protocol::{
    vpn::{ClientVpn, ServerVpn},
    ClientMessage, DaemonMessage,
};
use mirrord_vpn::{agent::VpnAgent, config::VpnConfig, error::VpnError, tunnel::VpnTunnel};
use tokio::signal;

use crate::{
    config::VpnArgs,
    connection::create_and_connect,
    error::{CliError, Result},
};

#[allow(clippy::indexing_slicing)]
pub async fn vpn_command(args: VpnArgs) -> Result<()> {
    let mut progress = ProgressTracker::from_env("mirrord vpn");

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

    let mut sub_progress = progress.subtask("fetching vpn info");

    let configmap_api = kube::Api::<ConfigMap>::namespaced(client, "kube-system");

    // TODO: this may fail but
    let Some(vpn_config) = VpnConfig::from_configmaps(&configmap_api).await else {
        sub_progress.failure(Some(
            "unable to lookup relevant configmaps to create our vpn config",
        ));

        return Ok(());
    };

    sub_progress.success(None);

    let mut sub_progress = progress.subtask("create agent");

    let (_, connection) = create_and_connect(&config, &mut sub_progress, &mut analytics)
        .await
        .inspect_err(|_| analytics.set_error(AnalyticsError::AgentConnection))?;

    sub_progress.success(None);

    let mut vpn_agnet = VpnAgent::new(connection.sender, connection.receiver);

    let Some(ServerVpn::NetworkConfiguration(network)) = vpn_agnet
        .send_and_get_response(
            ClientMessage::Vpn(ClientVpn::GetNetworkConfiguration),
            |message| match message {
                DaemonMessage::Vpn(response) => Some(response),
                _ => None,
            },
        )
        .await
        .map_err(VpnError::from)?
    else {
        return Err(VpnError::AgentUnexpcetedResponse.into());
    };

    tracing::debug!(?network, "loaded vpn network configuration");

    let mut sub_progress = progress.subtask("create tun socket");

    let vpn_socket = mirrord_vpn::socket::create_vpn_socket(&network);

    sub_progress.success(None);

    #[cfg(not(target_os = "macos"))]
    let linux_guard =
        mirrord_vpn::linux::mount_linux(&vpn_config, &network, &mut vpn_agnet).await?;

    #[cfg(target_os = "macos")]
    let macos_guard = mirrord_vpn::macos::mount_macos(&vpn_config, &network).await?;

    progress.success(None);

    let vpn_tunnel = VpnTunnel::new(vpn_agnet, vpn_socket);

    tokio::select! {
        _ = vpn_tunnel.start() => {}
        _ = signal::ctrl_c() => {}
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
