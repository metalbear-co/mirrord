use std::{path::Path, time::Duration};

use futures::{SinkExt, StreamExt};
use mirrord_analytics::NullReporter;
use mirrord_auth::credential_store::CredentialStore;
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::{client::OperatorApi, crd::NewOperatorFeature};
use mirrord_progress::{NullProgress, Progress, ProgressTracker};
use mirrord_protocol::{ClientMessage, DaemonMessage};
use mirrord_protocol_api::client::ProtocolConnector;
use tokio::time::Instant;
use tracing::Level;

use crate::{
    CliError, CliResult, DiagnoseArgs, DiagnoseCommand, connection::create_and_connect,
    connector::AgentConnection, util::remove_proxy_env,
};

/// Sends a ping the connection and expects a pong.
async fn ping(connection: &mut AgentConnection) -> CliResult<()> {
    connection.send(ClientMessage::Ping).await?;

    loop {
        let result = match connection.next().await {
            Some(Ok(DaemonMessage::Pong)) => Ok(()),
            Some(Ok(DaemonMessage::OperatorPing(id))) => {
                connection.send(ClientMessage::OperatorPong(id)).await?;
                Ok(())
            }
            Some(Ok(DaemonMessage::LogMessage(..))) => continue,
            Some(Ok(DaemonMessage::Close(message))) => Err(CliError::PingPongFailed(format!(
                "agent closed connection with message: {message}"
            ))),
            Some(Ok(message)) => Err(CliError::PingPongFailed(format!(
                "agent sent an unexpected message: {message:?}"
            ))),
            Some(Err(err)) => Err(CliError::PingPongFailed(format!(
                "agent connection error: {err}"
            ))),
            None => Err(CliError::PingPongFailed(
                "agent unexpectedly closed connection".to_owned(),
            )),
        };

        return result;
    }
}

/// Establishes the connection used to diagnose latency.
///
/// When the operator advertises [`NewOperatorFeature::DiagnosticPing`], connects to its no-session
/// ping endpoint through which the operator answers `ClientMessage::Ping` with
/// `DaemonMessage::Pong` directly without creating any session or agent.
///
/// Otherwise (older operator or OSS) falls back to [`create_and_connect`], which starts a
/// targetless session or spawns an agent as before.
async fn diagnose_connect(
    config: &mut LayerConfig,
    progress: &mut ProgressTracker,
    analytics: &mut NullReporter,
) -> CliResult<AgentConnection> {
    if config.operator != Some(false)
        && let Some(api) = OperatorApi::try_new(config, analytics, &NullProgress).await?
        && api
            .operator()
            .spec
            .supported_features()
            .contains(&NewOperatorFeature::DiagnosticPing)
    {
        let connection = api
            .with_client_certificate(analytics, progress, config)
            .await
            .into_certified()?
            .connect_diagnostic_ping()
            .await?;

        return Ok(AgentConnection::Operator(Box::new(connection)));
    }

    let mut analytics = NullReporter::default();
    let mut connector = create_and_connect(config, progress, &mut analytics, None, None, None)
        .await?
        .connector;

    let connection = connector
        .connect(progress)
        .await
        .map_err(|err| CliError::InitialAgentCommFailed(err.to_string()))?;

    Ok(connection)
}

/// Runs 100 ping/pong round trips over `connection` and reports min/max/avg latency.
async fn run_latency_pings<P: Progress>(
    connection: &mut AgentConnection,
    progress: &mut P,
) -> CliResult<()> {
    let mut statistics: Vec<Duration> = Vec::new();

    // ignore first ping as it's part of the initialization.
    ping(connection).await?;
    // run 100 iterations
    for i in 0..100 {
        let start = Instant::now();
        ping(connection).await?;
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

/// Runs pings to diagnose network latency, avoiding a full session when the operator supports it.
#[tracing::instrument(level = Level::TRACE, ret)]
async fn diagnose_latency(config: Option<&Path>) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("mirrord network diagnosis");

    let mut context = ConfigContext::default().override_env_opt(LayerConfig::FILE_PATH_ENV, config);
    let mut config = LayerConfig::resolve(&mut context)?;

    if !config.use_proxy {
        remove_proxy_env();
    }

    let mut analytics = NullReporter::default();
    let mut connection = diagnose_connect(&mut config, &mut progress, &mut analytics).await?;

    run_latency_pings(&mut connection, &mut progress).await?;

    Ok(())
}

/// Print the user fingerprints stored in the local credentials file.
///
/// Each entry corresponds to an operator license the machine has authenticated against. The
/// fingerprint is what the operator uses as `client_hash` to identify users (e.g. seat counting).
async fn diagnose_license() -> CliResult<()> {
    let store = CredentialStore::load_from_default_path().await?;
    let mut count = 0usize;
    for (operator_fp, user_fp) in store.user_fingerprints() {
        println!("operator: {operator_fp}");
        println!("  user fingerprint: {user_fp}");
        count += 1;
    }
    if count == 0 {
        println!("No credentials found in ~/.mirrord/credentials.");
    }
    Ok(())
}

/// Handle commands related to the operator `mirrord diagnose ...`
pub(crate) async fn diagnose_command(args: DiagnoseArgs) -> CliResult<()> {
    match args.command {
        DiagnoseCommand::Latency { config_file } => diagnose_latency(config_file.as_deref()).await,
        DiagnoseCommand::License => diagnose_license().await,
    }
}
