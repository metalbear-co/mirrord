//! Internal proxy is accepting connection from local layers and forward it to agent
//! while having 1:1 relationship - each layer connection is another agent connection.
//!
//! This might be changed later on.
//!
//! The main advantage of this design is that we remove kube logic from the layer itself,
//! thus eliminating bugs that happen due to mix of remote env vars in our code
//! (previously was solved using envguard which wasn't good enough)
//!
//! The proxy will either directly connect to an existing agent (currently only used for tests),
//! or let the [`OperatorApi`](mirrord_operator::client::OperatorApi) handle the connection.

use std::{
    env, io,
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};

use mirrord_analytics::{AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::LayerConfig;
use mirrord_intproxy::{
    agent_conn::{AgentConnectInfo, AgentConnection},
    error::IntProxyError,
    IntProxy,
};
use mirrord_protocol::{ClientMessage, DaemonMessage, LogLevel, LogMessage};
use nix::sys::resource::{setrlimit, Resource};
use tokio::net::TcpListener;
use tracing::{warn, Level};

use crate::{
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    error::{CliResult, InternalProxyError},
    execution::MIRRORD_EXECUTION_KIND_ENV,
    logging::init_intproxy_tracing_registry,
    util::{create_listen_socket, detach_io},
};

/// Print the address for the caller (mirrord cli execution flow) so it can pass it
/// back to the layer instances via env var.
fn print_addr(listener: &TcpListener) -> io::Result<()> {
    let addr = listener.local_addr()?;
    println!("{addr}\n");
    Ok(())
}

/// Main entry point for the internal proxy.
/// It listens for inbound layer connect and forwards to agent.
pub(crate) async fn proxy(
    listen_port: u16,
    watch: drain::Watch,
) -> CliResult<(), InternalProxyError> {
    let config = LayerConfig::recalculate_from_env()?;

    init_intproxy_tracing_registry(&config)?;
    tracing::info!(?config, "internal_proxy starting");

    // According to https://wilsonmar.github.io/maximum-limits/ this is the limit on macOS
    // so we assume Linux can be higher and set to that.
    if let Err(error) = setrlimit(Resource::RLIMIT_NOFILE, 12288, 12288) {
        warn!(?error, "Failed to set the file descriptor limit");
    }

    let agent_connect_info = match env::var(AGENT_CONNECT_INFO_ENV_KEY) {
        Ok(var) => {
            let deserialized = serde_json::from_str(&var)
                .map_err(|e| InternalProxyError::DeseralizeConnectInfo(var, e))?;
            Some(deserialized)
        }
        Err(..) => None,
    };

    let execution_kind = std::env::var(MIRRORD_EXECUTION_KIND_ENV)
        .ok()
        .and_then(|execution_kind| execution_kind.parse().ok())
        .unwrap_or_default();

    let mut analytics = if config.internal_proxy.container_mode {
        AnalyticsReporter::only_error(config.telemetry, execution_kind, watch)
    } else {
        AnalyticsReporter::new(config.telemetry, execution_kind, watch)
    };
    (&config).collect_analytics(analytics.get_mut());

    // The agent is spawned and our parent process already established a connection.
    // However, the parent process (`exec` or `ext` command) is free to exec/exit as soon as it
    // reads the TCP listener address from our stdout. We open our own connection with the agent
    // **before** this happens to ensure that the agent does not prematurely exit.
    // We also perform initial ping pong round to ensure that k8s runtime actually made connection
    // with the agent (it's a must, because port forwarding may be done lazily).
    let agent_conn = connect_and_ping(&config, agent_connect_info, &mut analytics).await?;

    // Let it assign address for us then print it for the user.
    let listener = create_listen_socket(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), listen_port))
        .map_err(InternalProxyError::ListenerSetup)?;
    print_addr(&listener).map_err(InternalProxyError::ListenerSetup)?;

    if !config.internal_proxy.container_mode {
        unsafe { detach_io() }.map_err(InternalProxyError::SetSid)?;
    }

    let first_connection_timeout = Duration::from_secs(config.internal_proxy.start_idle_timeout);
    let consecutive_connection_timeout = Duration::from_secs(config.internal_proxy.idle_timeout);

    IntProxy::new_with_connection(
        agent_conn,
        listener,
        config.experimental.readonly_file_buffer,
        Duration::from_millis(config.experimental.idle_local_http_connection_timeout),
        config.feature.network.incoming.https_delivery,
    )
    .run(first_connection_timeout, consecutive_connection_timeout)
    .await
    .map_err(InternalProxyError::from)
    .inspect_err(|error| {
        tracing::error!(%error, "Internal proxy encountered an error, exiting");
    })
}

/// Creates a connection with the agent and handles one round of ping pong.
#[tracing::instrument(level = Level::TRACE)]
pub(crate) async fn connect_and_ping(
    config: &LayerConfig,
    connect_info: Option<AgentConnectInfo>,
    analytics: &mut AnalyticsReporter,
) -> CliResult<AgentConnection, InternalProxyError> {
    let mut agent_conn = AgentConnection::new(config, connect_info, analytics)
        .await
        .map_err(IntProxyError::from)?;

    agent_conn
        .agent_tx
        .send(ClientMessage::Ping)
        .await
        .map_err(|_| {
            InternalProxyError::InitialPingPongFailed(
                "agent closed connection before ping".to_string(),
            )
        })?;

    loop {
        match agent_conn.agent_rx.recv().await {
            Some(DaemonMessage::Pong) => break Ok(agent_conn),
            Some(DaemonMessage::LogMessage(LogMessage {
                level: LogLevel::Error,
                message,
            })) => {
                tracing::error!("agent log: {message}");
            }
            Some(DaemonMessage::LogMessage(LogMessage {
                level: LogLevel::Warn,
                message,
            })) => {
                tracing::warn!("agent log: {message}");
            }
            Some(DaemonMessage::Close(reason)) => {
                break Err(InternalProxyError::InitialPingPongFailed(format!(
                    "agent closed connection with message: {reason}"
                )));
            }
            Some(message) => {
                break Err(InternalProxyError::InitialPingPongFailed(format!(
                    "agent sent an unexpected message: {message:?}"
                )));
            }
            None => {
                break Err(InternalProxyError::InitialPingPongFailed(
                    "agent unexpectedly closed connection".to_string(),
                ));
            }
        }
    }
}
