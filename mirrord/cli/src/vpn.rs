use k8s_openapi::api::core::v1::ConfigMap;
use mirrord_analytics::{AnalyticsError, NullReporter, Reporter};
use mirrord_config::{config::ConfigContext, LayerConfig};
use mirrord_kube::api::kubernetes::create_kube_config;
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_vpn::{agent::VpnAgent, config::VpnConfig, tunnel::VpnTunnel};
use tokio::signal;

use crate::{
    config::VpnArgs,
    connection::create_and_connect,
    error::{CliError, CliResult},
};

pub async fn vpn_command(args: VpnArgs) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord vpn");
    let mut analytics = NullReporter::default();

    let mut cfg_context = ConfigContext::default()
        .override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file)
        .override_env_opt("MIRRORD_TARGET_NAMESPACE", args.namespace);
    let mut config = LayerConfig::resolve(&mut cfg_context)?;
    config.agent.privileged = true;

    let client = create_kube_config(
        config.accept_invalid_certificates,
        config.kubeconfig.clone(),
        config.kube_context.clone(),
    )
    .await
    .and_then(|config| kube::Client::try_from(config).map_err(From::from))
    .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateKubeApiFailed))?;

    let mut sub_progress = progress.subtask("fetching vpn info");

    let configmap_api = kube::Api::<ConfigMap>::namespaced(client, "kube-system");

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

    let mut vpn_agnet = VpnAgent::try_create(connection.sender, connection.receiver).await?;

    let network = vpn_agnet.get_network_configuration().await?;

    tracing::debug!(?network, "loaded vpn network configuration");

    let mut sub_progress = progress.subtask("create tun socket");

    let vpn_socket = mirrord_vpn::socket::create_vpn_socket(&network);

    sub_progress.success(None);

    #[cfg(not(target_os = "macos"))]
    let _linux_guard =
        mirrord_vpn::linux::mount_linux(&vpn_config, &network, &mut vpn_agnet).await?;

    #[cfg(target_os = "macos")]
    let _macos_guard = mirrord_vpn::macos::mount_macos(&vpn_config, &network)?;

    progress.success(None);

    let vpn_tunnel = VpnTunnel::new(vpn_agnet, vpn_socket);

    tokio::select! {
        _ = vpn_tunnel.start() => {}
        _ = signal::ctrl_c() => {}
    }

    Ok(())
}
