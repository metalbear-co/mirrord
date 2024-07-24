use std::{
    fs::OpenOptions,
    io,
    io::Write,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use futures::{SinkExt, StreamExt};
use local_ip_address::{local_ip, local_ipv6};
use mirrord_analytics::{AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::LayerConfig;
use mirrord_intproxy::{
    agent_conn::{AgentConnectInfo, AgentConnection},
    error::IntProxyError,
};
use mirrord_protocol::{ClientMessage, DaemonCodec, DaemonMessage, LogLevel, LogMessage};
use nix::libc;
use tokio::net::TcpListener;
use tokio_util::codec::Framed;
use tracing_subscriber::EnvFilter;

use crate::{
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    error::{ExternalProxyError, Result},
};

unsafe fn redirect_fd_to_dev_null(fd: libc::c_int) {
    let devnull_fd = libc::open(b"/dev/null\0" as *const [u8; 10] as _, libc::O_RDWR);
    libc::dup2(devnull_fd, fd);
    libc::close(devnull_fd);
}

unsafe fn detach_io() -> Result<(), ExternalProxyError> {
    // Create a new session for the proxy process, detaching from the original terminal.
    // This makes the process not to receive signals from the "mirrord" process or it's parent
    // terminal fixes some side effects such as https://github.com/metalbear-co/mirrord/issues/1232
    nix::unistd::setsid().map_err(ExternalProxyError::SetSid)?;

    // flush before redirection
    {
        // best effort
        let _ = std::io::stdout().lock().flush();
    }
    for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        redirect_fd_to_dev_null(fd);
    }
    Ok(())
}

/// Print the port for the caller (mirrord cli execution flow) so it can pass it
/// back to the layer instances via env var.
fn print_addr(listener: &TcpListener) -> io::Result<()> {
    let addr = listener.local_addr()?;

    let connect_tcp = SocketAddr::new(
        match addr.ip() {
            IpAddr::V4(_) => local_ip().unwrap_or_else(|_| Ipv4Addr::LOCALHOST.into()),
            IpAddr::V6(_) => local_ipv6().unwrap_or_else(|_| Ipv6Addr::LOCALHOST.into()),
        },
        addr.port(),
    );

    println!("{connect_tcp}\n");
    Ok(())
}

/// Creates a connection with the agent and handles one round of ping pong.
async fn connect_and_ping(
    config: &LayerConfig,
    connect_info: Option<AgentConnectInfo>,
    analytics: &mut AnalyticsReporter,
) -> Result<AgentConnection, ExternalProxyError> {
    let mut agent_conn = AgentConnection::new(config, connect_info, analytics)
        .await
        .map_err(IntProxyError::from)?;

    agent_conn
        .agent_tx
        .send(ClientMessage::Ping)
        .await
        .map_err(|_| {
            ExternalProxyError::InitialPingPongFailed(
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
                break Err(ExternalProxyError::InitialPingPongFailed(format!(
                    "agent closed connection with message: {reason}"
                )));
            }
            Some(message) => {
                break Err(ExternalProxyError::InitialPingPongFailed(format!(
                    "agent sent an unexpected message: {message:?}"
                )));
            }
            None => {
                break Err(ExternalProxyError::InitialPingPongFailed(
                    "agent unexpectedly closed connection".to_string(),
                ));
            }
        }
    }
}

pub async fn proxy(watch: drain::Watch) -> Result<()> {
    let config = LayerConfig::from_env()?;

    if let Some(log_destination) = config.internal_proxy.log_destination.as_ref() {
        let output_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_destination)
            .map_err(|e| ExternalProxyError::OpenLogFile(log_destination.clone(), e))?;

        let tracing_registry = tracing_subscriber::fmt()
            .with_writer(output_file)
            .with_ansi(false);

        if let Some(log_level) = config.internal_proxy.log_level.as_ref() {
            tracing_registry
                .with_env_filter(EnvFilter::builder().parse_lossy(log_level))
                .init();
        } else {
            tracing_registry.init();
        }
    }

    let agent_connect_info = match std::env::var(AGENT_CONNECT_INFO_ENV_KEY) {
        Ok(var) => {
            let deserialized = serde_json::from_str(&var)
                .map_err(|e| ExternalProxyError::DeseralizeConnectInfo(var, e))?;
            Some(deserialized)
        }
        Err(..) => None,
    };
    let mut analytics = AnalyticsReporter::new(config.telemetry, watch);
    (&config).collect_analytics(analytics.get_mut());

    let listener = TcpListener::bind(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0))
        .await
        .map_err(ExternalProxyError::ListenerSetup)?;
    print_addr(&listener).map_err(ExternalProxyError::ListenerSetup)?;

    unsafe {
        detach_io()?;
    }

    while let Ok((socket, _)) = listener.accept().await {
        let mut socket = Framed::new(socket, DaemonCodec::default());

        let mut agent_conn =
            connect_and_ping(&config, agent_connect_info.clone(), &mut analytics).await?;

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    client_message = socket.next() => {
                        if let Some(Ok(client_message)) = client_message {
                            let _ = agent_conn.agent_tx.send(client_message).await;
                        } else {
                            break;
                        }
                    }
                    daemon_message = agent_conn.agent_rx.recv() => {
                        if let Some(daemon_message) = daemon_message {
                            let _ = socket.send(daemon_message).await;
                        } else {
                            break;
                        }
                    }
                }
            }
        });
    }

    Ok(())
}
