use mirrord_analytics::NullReporter;
use mirrord_config::{config::{ConfigContext, MirrordConfig}, LayerConfig, LayerFileConfig};
use mirrord_progress::ProgressTracker;

use crate::{connection::create_and_connect, util::remove_proxy_env, DiagnoseArgs, DiagnoseCommand, Result};



/// Create a targetless session and run pings to diagnose network latency.
#[tracing::instrument(level = "trace", ret)]
async fn diagnose_latency(config: Option<String>) -> Result<()> {
    let mut progress = ProgressTracker::from_env("mirrord network diagnosis");
    
    let mut cfg_context = ConfigContext::default();
    let config = if let Some(path) = config {
        LayerFileConfig::from_path(path)?.generate_config(&mut cfg_context)
    } else {
        LayerFileConfig::default().generate_config(&mut cfg_context)
    }?;

    if !config.use_proxy {
        remove_proxy_env();
    }

    let mut analytics = NullReporter::default();
    let (connect_info, mut connection) = create_and_connect(config, progress, &mut analytics).await?;
    
    Ok(())

}

/// Handle commands related to the operator `mirrord diagnose ...`
pub(crate) async fn diagnose_command(args: DiagnoseArgs) -> Result<()> {
    match args.command {
        DiagnoseCommand::Latency { config_file } => diagnose_latency(config_file).await,
    }
}