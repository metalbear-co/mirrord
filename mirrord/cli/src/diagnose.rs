use std::{path::Path, time::Duration};

use mirrord_analytics::NullReporter;
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    LayerFileConfig,
};
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use tokio::{sync::mpsc, time::Instant};
use tracing::Level;

use crate::{
    connection::create_and_connect, util::remove_proxy_env, CliError, DiagnoseArgs,
    DiagnoseCommand, Result,
};

/// Sends a ping the connection and expects a pong.
async fn ping(
    sender: &mpsc::Sender<ClientMessage>,
    receiver: &mut mpsc::Receiver<DaemonMessage>,
) -> Result<()> {
    sender.send(ClientMessage::Ping).await.map_err(|_| {
        CliError::PingPongFailed(
            "failed to send ping message - agent unexpectedly closed connection".to_string(),
        )
    })?;

    loop {
        let result = match receiver.recv().await {
            Some(DaemonMessage::Pong) => Ok(()),
            Some(DaemonMessage::LogMessage(..)) => continue,
            Some(DaemonMessage::Close(message)) => Err(CliError::PingPongFailed(format!(
                "agent closed connection with message: {message}"
            ))),
            Some(message) => Err(CliError::PingPongFailed(format!(
                "agent sent an unexpected message: {message:?}"
            ))),
            None => Err(CliError::PingPongFailed(
                "agent unexpectedly closed connection".to_string(),
            )),
        };

        return result;
    }
}

/// Create a targetless session and run pings to diagnose network latency.
#[tracing::instrument(level = Level::TRACE, ret)]
async fn diagnose_latency(config: Option<&Path>) -> Result<()> {
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
    let (_, mut connection) = create_and_connect(&config, &mut progress, &mut analytics).await?;

    let mut statistics: Vec<Duration> = Vec::new();

    // ignore first ping as it's part of the initialization.
    ping(&connection.sender, &mut connection.receiver).await?;
    // run 100 iterations
    for i in 0..100 {
        let start = Instant::now();
        ping(&connection.sender, &mut connection.receiver).await?;
        let elapsed = start.elapsed();
        progress.info(
            format!(
                "{i}/100 iterations completed, last iteration took {}ms",
                elapsed.as_millis()
            )
            .as_str(),
        );
        statistics.push(elapsed);
    }

    let min = statistics.iter().min().expect("never empty").as_millis();
    let max = statistics.iter().max().expect("never empty").as_millis();
    let avg = (statistics.iter().sum::<Duration>() / statistics.len() as u32).as_millis();
    progress.success(Some(
        format!(
            "Latency statistics: min={}ms, max={}ms, avg={}ms",
            min, max, avg
        )
        .as_str(),
    ));

    Ok(())
}

/// Handle commands related to the operator `mirrord diagnose ...`
pub(crate) async fn diagnose_command(args: DiagnoseArgs) -> Result<()> {
    match args.command {
        DiagnoseCommand::Latency { config_file } => diagnose_latency(config_file.as_deref()).await,
    }
}
