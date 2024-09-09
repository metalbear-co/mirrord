//!
//! External proxy to pipe communication from intproxy to agent
//!
//! ```text
//!                    ┌────────────────┐         
//!                k8s │ mirrord agent  │         
//!                    └─────┬────▲─────┘         
//!                          │    │               
//!                          │    │               
//!                    ┌─────▼────┴─────┐         
//!     container host │ external proxy │         
//!                    └─────┬────▲─────┘         
//!                          │    │               
//!                          │    │               
//!                    ┌─────▼────┴─────┐◄──────┐
//!  sidecar container │ internal proxy │       │
//!                    └──┬─────────────┴──┐    │
//!         run container │ mirrord-layer  ├────┘
//!                       └────────────────┘      
//! ```

use std::{
    fs::OpenOptions,
    io,
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use mirrord_analytics::{AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::LayerConfig;
use mirrord_intproxy::agent_conn::AgentConnection;
use mirrord_protocol::DaemonCodec;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tracing::Level;
use tracing_subscriber::EnvFilter;

use crate::{
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    error::{ExternalProxyError, Result},
    execution::MIRRORD_EXECUTION_KIND_ENV,
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

pub async fn proxy(watch: drain::Watch) -> Result<()> {
    let config = LayerConfig::from_env()?;

    tracing::info!(?config, "external_proxy starting");

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

    let agent_connect_info = std::env::var(AGENT_CONNECT_INFO_ENV_KEY)
        .ok()
        .map(|var| {
            serde_json::from_str(&var)
                .map_err(|e| ExternalProxyError::DeseralizeConnectInfo(var, e))
        })
        .transpose()?;

    let execution_kind = std::env::var(MIRRORD_EXECUTION_KIND_ENV)
        .ok()
        .and_then(|execution_kind| execution_kind.parse().ok())
        .unwrap_or_default();

    let mut analytics = AnalyticsReporter::new(config.telemetry, execution_kind, watch);
    (&config).collect_analytics(analytics.get_mut());

    // let tls_acceptor = create_external_proxy_tls_acceptor(&config).await?;
    let listener = create_listen_socket(SocketAddr::new(config.external_proxy.listen, 0))
        .map_err(ExternalProxyError::ListenerSetup)?;
    print_addr(&listener).map_err(ExternalProxyError::ListenerSetup)?;

    unsafe { detach_io() }.map_err(ExternalProxyError::SetSid)?;

    let cancellation_token = CancellationToken::new();
    let connections = Arc::new(AtomicUsize::new(0));
    let idle_timeout = config.external_proxy.idle_timeout;

    let mut initial_connection_timeout = Box::pin(tokio::time::sleep(Duration::from_secs(
        config.external_proxy.start_idle_timeout,
    )));

    loop {
        tokio::select! {
            conn = listener.accept() => {
                if let Ok((stream, peer_addr)) = conn {
                    tracing::debug!(?peer_addr, "new connection");

                    let connections = connections.clone();
                    let cancellation_token = cancellation_token.clone();
                    let connection_cancelation_token = cancellation_token.child_token();

                    let agent_conn = connect_and_ping(&config, agent_connect_info.clone(), &mut analytics).await.inspect_err(|_| cancellation_token.cancel())?;
                    connections.fetch_add(1, Ordering::Relaxed);

                    let fut = async move {
                        handle_connection(stream, peer_addr, agent_conn, connection_cancelation_token).await;

                        tracing::debug!(?peer_addr, "closed connection");

                        Ok::<(), std::io::Error>(())
                    };

                    tokio::spawn(async move {
                        if let Err(error) = fut.await {
                            tracing::error!(%error, "error handling proxy connection");
                        }

                        // Simple patch to allow idle_timeout by wating the timout period on each request before maybe calling the cancellation_token if no connections left
                        tokio::time::sleep(Duration::from_secs(idle_timeout)).await;

                        if connections.fetch_sub(1, Ordering::Relaxed) == 1 {
                            cancellation_token.cancel();

                            tracing::debug!(?peer_addr, "final connection, closing listener");
                        }
                    });
                } else {
                    break;
                }
            }

            _ = initial_connection_timeout.as_mut(), if connections.load(Ordering::Relaxed) == 0 => {
                tracing::debug!("closing listener due to initial connection timeout");

                break;
            }

            _ = cancellation_token.cancelled() => {
                tracing::debug!("closing listener due to cancellation_token");

                break;
            }
        }
    }

    Ok(())
}

#[tracing::instrument(level = Level::TRACE, skip(agent_conn))]
async fn handle_connection(
    stream: TcpStream,
    peer_addr: SocketAddr,
    mut agent_conn: AgentConnection,
    cancellation_token: CancellationToken,
) {
    let mut stream = actix_codec::Framed::new(stream, DaemonCodec::default());

    loop {
        tokio::select! {
            client_message = stream.next() => {
                match client_message {
                    Some(Ok(client_message)) => {
                        if let Err(error) = agent_conn.agent_tx.send(client_message).await {
                            tracing::error!(?peer_addr, %error, "unable to send message to agent");

                            break;
                        }
                    }
                    Some(Err(error)) => {
                        tracing::error!(?peer_addr, %error, "unable to recive message from intproxy");

                        break;
                    }
                    None => {
                        break;
                    }
                }
            }
            daemon_message = agent_conn.agent_rx.recv() => {
                if let Some(daemon_message) = daemon_message {
                    if let Err(error) = stream.send(daemon_message).await {
                        tracing::error!(?peer_addr, %error, "unable to send message to intproxy");

                        break;
                    }
                } else {
                    break;
                }
            }
            _ = cancellation_token.cancelled() => {
                tracing::debug!(?peer_addr, "closing connection due to cancellation_token");

                break;
            }
        }
    }
}
