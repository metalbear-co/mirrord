use mirrord_analytics::{AnalyticsReporter, CollectAnalytics, ExecutionKind, Reporter};
use mirrord_config::{config::ConfigContext, LayerConfig};
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_protocol::{
    tcp::{LayerTcp, NewTcpConnectionV1, TcpData},
    ClientMessage, DaemonMessage,
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::config::DumpArgs;
use crate::{
    connection::{create_and_connect, AgentConnection},
    error::{CliError, CliResult},
    util::get_user_git_branch,
};

/// Implements the `mirrord dump` command.
///
/// This command starts a mirrord session using the given config file and target arguments,
/// subscribes to mirror traffic from the specified ports, and prints any traffic that comes
/// to the screen with the connection ID.
pub async fn dump_command(args: &DumpArgs, watch: drain::Watch) -> CliResult<()> {
    // Set up configuration similar to exec command
    let mut cfg_context = ConfigContext::default().override_envs(args.params.as_env_vars());
    cfg_context.get_env(LayerConfig::FILE_PATH_ENV).ok();
    let mut config = LayerConfig::resolve(&mut cfg_context)?;

    let mut progress = ProgressTracker::from_env("mirrord dump");
    let mut analytics = AnalyticsReporter::new(config.telemetry, ExecutionKind::Dump, watch);

    // Collect analytics
    (&config).collect_analytics(analytics.get_mut());

    // Get git branch for analytics
    let branch_name = get_user_git_branch().await;

    // Create connection to the agent
    let (_connection_info, connection) =
        create_and_connect(&mut config, &mut progress, &mut analytics, branch_name).await?;

    progress.success(Some("Connected to agent"));

    // Start the dump session
    dump_session(connection, args.ports.clone(), &mut progress).await?;

    Ok(())
}

/// Handles the actual dump session by subscribing to ports and listening for traffic.
async fn dump_session(
    connection: AgentConnection,
    ports: Vec<u16>,
    progress: &mut ProgressTracker,
) -> CliResult<()> {
    let (tx, mut rx) = mpsc::channel::<DaemonMessage>(1000);

    // Spawn a task to handle incoming messages from the agent
    let mut connection_rx = connection.receiver;
    tokio::spawn(async move {
        while let Some(message) = connection_rx.recv().await {
            if let Err(e) = tx.send(message).await {
                warn!("Failed to forward message: {}", e);
                break;
            }
        }
    });

    // Subscribe to all specified ports
    for port in &ports {
        let subscribe_message = ClientMessage::Tcp(LayerTcp::PortSubscribe(*port));

        if let Err(e) = connection.sender.send(subscribe_message).await {
            return Err(CliError::DumpError(format!(
                "Failed to subscribe to port {port}: {e}"
            )));
        }
        info!("Subscribed to port {} for mirroring", port);
    }

    progress.success(Some("Subscribed to all ports"));

    // Listen for incoming traffic
    info!("Listening for traffic on ports: {:?}", ports);
    progress.info("Listening for traffic... Press Ctrl+C to stop");

    while let Some(message) = rx.recv().await {
        match message {
            DaemonMessage::Tcp(mirrord_protocol::tcp::DaemonTcp::Data(TcpData {
                connection_id,
                bytes,
            })) => {
                // Print the data with connection ID
                println!("## Connection ID {connection_id}: {} bytes", bytes.len());
                if !bytes.is_empty() {
                    // Try to print as string, fallback to hex if not valid UTF-8
                    match std::str::from_utf8(&bytes) {
                        Ok(s) => println!("Data: {}", s),
                        Err(_) => println!("Data (hex): {:?}", bytes),
                    }
                }
            }
            DaemonMessage::Tcp(mirrord_protocol::tcp::DaemonTcp::NewConnectionV1(
                NewTcpConnectionV1 {
                    connection_id,
                    remote_address,
                    source_port,
                    destination_port,
                    local_address,
                },
            )) => {
                println!(
                    "## New connection established: Connection ID {connection_id} from {}:{} to {}:{}",
                    remote_address, source_port, local_address, destination_port
                );
            }
            DaemonMessage::Tcp(mirrord_protocol::tcp::DaemonTcp::Close(close)) => {
                println!("## Connection ID {} closed", close.connection_id);
            }
            _ => {
                debug!("Received other message: {:?}", message);
            }
        }
    }

    Ok(())
}
