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

mod db_portforwards;

#[cfg(not(target_os = "windows"))]
use std::os::unix::ffi::OsStrExt;
use std::{
    env, io,
    net::{Ipv4Addr, SocketAddr},
    ops::Not,
    time::Duration,
};

use mirrord_analytics::{AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::LayerConfig;
use mirrord_intproxy::{
    IntProxy,
    agent_conn::{AgentConnectInfo, AgentConnection},
    session_monitor::MonitorTx,
};
use mirrord_protocol::{ClientMessage, DaemonMessage, LogLevel, LogMessage};
#[cfg(unix)]
use mirrord_session_monitor_protocol::SessionInfo;
#[cfg(not(target_os = "windows"))]
use nix::sys::resource::{Resource, setrlimit};
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio_util::sync::CancellationToken;
use tracing::Level;
#[cfg(not(target_os = "windows"))]
use tracing::warn;

#[cfg(unix)]
use crate::kube::kube_client_from_layer_config;
#[cfg(not(target_os = "windows"))]
use crate::util::detach_io;
use crate::{
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    error::{CliResult, InternalProxyError},
    execution::MIRRORD_EXECUTION_KIND_ENV,
    user_data::UserData,
    util::create_listen_socket,
};

/// Print the address for the caller (mirrord cli execution flow) so it can pass it
/// back to the layer instances via env var.
fn print_addr(listener: &TcpListener) -> io::Result<()> {
    let addr = listener.local_addr()?;
    println!("{addr}\n");
    Ok(())
}

/// Starts the session monitor API server if enabled and on Unix, otherwise returns a
/// disabled [`MonitorTx`].
async fn start_session_monitor(config: &LayerConfig, is_operator: bool) -> MonitorTx {
    #[cfg(not(unix))]
    {
        let _ = (config, is_operator);
        MonitorTx::disabled()
    }

    #[cfg(unix)]
    {
        if !config.api {
            return MonitorTx::disabled();
        }

        let (tx, _rx) =
            tokio::sync::broadcast::channel::<mirrord_intproxy::session_monitor::MonitorEvent>(256);
        let proxy_monitor_tx = MonitorTx::from_sender(tx.clone());
        let api_monitor_tx = MonitorTx::from_sender(tx);

        let session_id =
            env::var("MIRRORD_SESSION_ID").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());

        let target_name = config
            .target
            .path
            .as_ref()
            .map(|t| t.to_string())
            .unwrap_or_else(|| "targetless".to_owned());

        let namespace = match &config.target.namespace {
            Some(namespace) => Some(namespace.clone()),
            None => match kube_client_from_layer_config(config).await {
                Ok(client) => Some(client.default_namespace().to_owned()),
                Err(error) => {
                    tracing::debug!(
                        ?error,
                        "Failed to resolve effective namespace from kube client"
                    );
                    None
                }
            },
        };

        let config_value = serde_json::to_value(config).unwrap_or(serde_json::Value::Null);

        let session_info = SessionInfo {
            session_id: session_id.clone(),
            key: Some(config.key.as_str().to_owned()),
            target: target_name,
            namespace,
            started_at: humantime::format_rfc3339(std::time::SystemTime::now()).to_string(),
            mirrord_version: env!("CARGO_PKG_VERSION").to_owned(),
            is_operator,
            processes: Vec::new(),
            port_subscriptions: Vec::new(),
            config: config_value,
        };

        let shutdown = CancellationToken::new();

        tokio::spawn(async move {
            if let Err(error) = mirrord_intproxy::session_monitor::api::start_api_server(
                session_info,
                api_monitor_tx,
                shutdown,
            )
            .await
            {
                tracing::warn!(%error, "Session monitor API server failed");
            }
        });

        proxy_monitor_tx
    }
}

/// Main entry point for the internal proxy.
/// It listens for inbound layer connect and forwards to agent.
#[tracing::instrument(level = Level::INFO, skip_all, err)]
pub(crate) async fn proxy(
    config: LayerConfig,
    listen_port: u16,
    watch: drain::Watch,
    user_data: &UserData,
) -> Result<(), InternalProxyError> {
    tracing::info!(
        ?config,
        listen_port,
        version = env!("CARGO_PKG_VERSION"),
        "Starting mirrord-intproxy",
    );

    // According to https://wilsonmar.github.io/maximum-limits/ this is the limit on macOS
    // so we assume Linux can be higher and set to that.
    #[cfg(not(target_os = "windows"))]
    if let Err(error) = setrlimit(Resource::RLIMIT_NOFILE, 12288, 12288) {
        warn!(%error, "Failed to set the file descriptor limit");
    }

    let agent_connect_info = env::var_os(AGENT_CONNECT_INFO_ENV_KEY)
        .ok_or(InternalProxyError::MissingConnectInfo)
        .and_then(|var| {
            #[cfg(target_os = "windows")]
            let var = var.to_string_lossy();
            serde_json::from_slice(var.as_bytes()).map_err(|error| {
                InternalProxyError::DeseralizeConnectInfo(
                    String::from_utf8_lossy(var.as_bytes()).into_owned(),
                    error,
                )
            })
        })?;

    let execution_kind = std::env::var(MIRRORD_EXECUTION_KIND_ENV)
        .ok()
        .and_then(|execution_kind| execution_kind.parse().ok())
        .unwrap_or_default();
    let container_mode = crate::util::intproxy_container_mode();

    let mut analytics = if container_mode {
        AnalyticsReporter::only_error(
            config.telemetry,
            execution_kind,
            watch,
            user_data.machine_id(),
        )
    } else {
        AnalyticsReporter::new(
            config.telemetry,
            execution_kind,
            watch,
            user_data.machine_id(),
        )
    };
    (&config).collect_analytics(analytics.get_mut());

    let operator_session_id = if let AgentConnectInfo::Operator(session) = &agent_connect_info {
        Some(session.id())
    } else {
        None
    };

    // The agent is spawned and our parent process already established a connection.
    // However, the parent process (`exec` or `ext` command) is free to exec/exit as soon as it
    // reads the TCP listener address from our stdout. We open our own connection with the agent
    // **before** this happens to ensure that the agent does not prematurely exit.
    // We also perform initial ping pong round to ensure that k8s runtime actually made connection
    // with the agent (it's a must, because port forwarding may be done lazily).
    let is_operator = matches!(&agent_connect_info, AgentConnectInfo::Operator(_));
    let mut agent_conn = connect_and_ping(&config, agent_connect_info, &mut analytics).await?;

    if config.feature.db_branches.is_empty().not()
        && let Some(session_id) = operator_session_id
        && let Err(err) = db_portforwards::setup(
            &config.feature.db_branches,
            &mut agent_conn,
            session_id,
            config.key.as_str(),
        )
        .await
    {
        tracing::warn!(%err, "failed to set up DB branch port forwards, continuing without them");
    }

    // Let it assign address for us then print it for the user.
    let listener = create_listen_socket(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), listen_port))
        .map_err(InternalProxyError::ListenerSetup)?;
    print_addr(&listener).map_err(InternalProxyError::ListenerSetup)?;

    #[cfg(not(target_os = "windows"))]
    if container_mode.not() {
        unsafe { detach_io() }.map_err(InternalProxyError::SetSid)?;
    }

    let first_connection_timeout = Duration::from_secs(config.internal_proxy.start_idle_timeout);
    let consecutive_connection_timeout = Duration::from_secs(config.internal_proxy.idle_timeout);
    let process_logging_interval =
        Duration::from_secs(config.internal_proxy.process_logging_interval);

    let monitor_tx = start_session_monitor(&config, is_operator).await;

    IntProxy::new_with_connection(
        agent_conn,
        listener,
        config.feature.fs.readonly_file_buffer,
        config
            .feature
            .network
            .incoming
            .tls_delivery
            .or(config.feature.network.incoming.https_delivery)
            .unwrap_or_default(),
        process_logging_interval,
        &config.experimental,
        monitor_tx,
    )
    .run(first_connection_timeout, consecutive_connection_timeout)
    .await
    .map_err(From::from)
}

/// Creates a connection with the agent and handles one round of ping pong.
#[tracing::instrument(level = Level::TRACE, skip(config, analytics))]
pub(crate) async fn connect_and_ping(
    config: &LayerConfig,
    connect_info: AgentConnectInfo,
    analytics: &mut AnalyticsReporter,
) -> CliResult<AgentConnection, InternalProxyError> {
    let mut agent_conn = AgentConnection::new(config, connect_info, analytics).await?;

    agent_conn.connection.send(ClientMessage::Ping).await;

    loop {
        match agent_conn.connection.recv().await {
            Some(DaemonMessage::Pong) => break Ok(agent_conn),
            Some(DaemonMessage::OperatorPing(id)) => {
                agent_conn
                    .connection
                    .send(ClientMessage::OperatorPong(id))
                    .await;
            }
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

            message @ Some(DaemonMessage::UdpOutgoing(_))
            | message @ Some(DaemonMessage::Tcp(_))
            | message @ Some(DaemonMessage::TcpSteal(_))
            | message @ Some(DaemonMessage::TcpOutgoing(_))
            | message @ Some(DaemonMessage::File(_))
            | message @ Some(DaemonMessage::LogMessage(_))
            | message @ Some(DaemonMessage::GetEnvVarsResponse(_))
            | message @ Some(DaemonMessage::GetAddrInfoResponse(_))
            | message @ Some(DaemonMessage::PauseTarget(_))
            | message @ Some(DaemonMessage::SwitchProtocolVersionResponse(_))
            | message @ Some(DaemonMessage::Vpn(_))
            | message @ Some(DaemonMessage::ReverseDnsLookup(_)) => {
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
