use k8s_openapi::api::core::v1::ConfigMap;
use kube::client::ClientBuilder;
use mirrord_analytics::{AnalyticsError, NullReporter, Reporter};
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_kube::{api::kubernetes::create_kube_config, retry::RetryKube};
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_vpn::{agent::VpnAgent, config::VpnConfig, tunnel::VpnTunnel};
use tokio::signal;
use tower::{buffer::BufferLayer, retry::RetryLayer};

use crate::{
    config::VpnArgs,
    connection::create_and_connect,
    error::{CliError, CliResult},
    util::get_user_git_branch,
};

pub async fn vpn_command(args: VpnArgs) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord vpn");
    let mut analytics = NullReporter::default();

    let mut cfg_context = ConfigContext::default()
        .override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file)
        .override_env_opt("MIRRORD_TARGET_NAMESPACE", args.namespace);

    let mut layer_config = LayerConfig::resolve(&mut cfg_context)?;
    layer_config.agent.privileged = true;

    let client = create_kube_config(
        layer_config.accept_invalid_certificates,
        layer_config.kubeconfig.clone(),
        layer_config.kube_context.clone(),
    )
    .await
    .and_then(|config| {
        Ok(ClientBuilder::try_from(config.clone())?
            .with_layer(&BufferLayer::new(1024))
            .with_layer(&RetryLayer::new(RetryKube::try_from(
                &layer_config.startup_retry,
            )?))
            .build())
    })
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

    let branch_name = get_user_git_branch().await;
    let (_, connection) = create_and_connect(
        &mut layer_config,
        &mut sub_progress,
        &mut analytics,
        branch_name,
    )
    .await
    .inspect_err(|_| analytics.set_error(AnalyticsError::AgentConnection))?;

    sub_progress.success(None);

    let mut vpn_agnet = VpnAgent::try_create(connection).await?;

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
