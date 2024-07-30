use std::{
    fs::OpenOptions,
    io,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use local_ip_address::local_ip;
use mirrord_analytics::{AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::LayerConfig;
use mirrord_intproxy::agent_conn::AgentConnection;
use mirrord_protocol::DaemonCodec;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::{codec::Framed, sync::CancellationToken};
use tracing_subscriber::EnvFilter;

use crate::{
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    error::{ExternalProxyError, Result},
    internal_proxy::connect_and_ping,
    util::{create_listen_socket, detach_io},
};

/// Print the address for the caller (mirrord cli execution flow) so it can pass it
/// back to the layer instances via env var.
fn print_addr(listener: &TcpListener) -> io::Result<()> {
    let addr = listener.local_addr()?;
    println!("{addr}\n");
    Ok(())
}

async fn handle_connection(socket: TcpStream, mut agent_conn: AgentConnection) {
    let mut socket = Framed::new(socket, DaemonCodec::default());

    loop {
        tokio::select! {
            client_message = socket.next() => {
                match client_message {
                    Some(Ok(client_message)) => {
                        if let Err(error) = agent_conn.agent_tx.send(client_message).await {
                            tracing::error!(%error, "unable to send message to agent");

                            break;
                        }
                    }
                    Some(Err(error)) => {
                        tracing::error!(%error, "unable to recive message from intproxy");

                        break;
                    }
                    None => {
                        break;
                    }
                }
            }
            daemon_message = agent_conn.agent_rx.recv() => {
                if let Some(daemon_message) = daemon_message {
                    if let Err(error) = socket.send(daemon_message).await {
                        tracing::error!(%error, "unable to send message to intproxy");

                        break;
                    }
                } else {
                    break;
                }
            }
        }
    }
}

pub async fn proxy(watch: drain::Watch) -> Result<()> {
    let config = LayerConfig::from_env()?;

    if let Some(log_destination) = config.external_proxy.log_destination.as_ref() {
        let output_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_destination)
            .map_err(|e| ExternalProxyError::OpenLogFile(log_destination.clone(), e))?;

        let tracing_registry = tracing_subscriber::fmt()
            .with_writer(output_file)
            .with_ansi(false);

        if let Some(log_level) = config.external_proxy.log_level.as_ref() {
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

    let listener = create_listen_socket(SocketAddr::new(
        local_ip().unwrap_or_else(|_| Ipv4Addr::UNSPECIFIED.into()),
        0,
    ))
    .map_err(ExternalProxyError::ListenerSetup)?;
    print_addr(&listener).map_err(ExternalProxyError::ListenerSetup)?;

    unsafe {
        detach_io().map_err(ExternalProxyError::SetSid)?;
    }

    let cancellation_token = CancellationToken::new();
    let connections = Arc::new(AtomicUsize::new(0));

    let mut initial_connection_timeout = Box::pin(tokio::time::sleep(Duration::from_secs(
        config.external_proxy.start_idle_timeout,
    )));

    loop {
        tokio::select! {
            conn = listener.accept() => {
                if let Ok((socket, addr)) = conn {
                    tracing::debug!(?addr, "new connection");

                    let agent_conn =
                        connect_and_ping(&config, agent_connect_info.clone(), &mut analytics).await?;

                    connections.fetch_add(1, Ordering::Relaxed);

                    tokio::spawn({
                        let connections = connections.clone();
                        let cancellation_token = cancellation_token.clone();

                        async move {
                            handle_connection(socket, agent_conn).await;

                            tracing::debug!(?addr, "closed connection");

                            if connections.fetch_sub(1, Ordering::Relaxed) == 1 {
                                cancellation_token.cancel();
                            }
                        }
                    });
                } else {
                    break;
                }
            }

            _ = initial_connection_timeout.as_mut(), if connections.load(Ordering::Relaxed) == 0 => {
                break;
            }
            _ = cancellation_token.cancelled() => {
                break;
            }
        }
    }

    Ok(())
}
