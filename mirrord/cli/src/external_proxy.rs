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
    io,
    net::{Ipv4Addr, SocketAddr},
    os::unix::ffi::OsStrExt,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use local_ip_address::local_ip;
use mirrord_analytics::{AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::{external_proxy::MIRRORD_EXTPROXY_TLS_SETUP_PEM, LayerConfig};
use mirrord_intproxy::agent_conn::{AgentConnectInfo, AgentConnection};
use mirrord_protocol::{ClientMessage, DaemonCodec, DaemonMessage, LogLevel, LogMessage};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::server::TlsStream;
use tokio_util::{either::Either, sync::CancellationToken};
use tracing::Level;

use crate::{
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    error::{CliResult, ExternalProxyError},
    execution::MIRRORD_EXECUTION_KIND_ENV,
    internal_proxy::connect_and_ping,
    util::{create_listen_socket, detach_io},
};

/// Print the address for the caller (mirrord cli execution flow) so it can pass it
/// back to the layer instances via env var.
fn print_addr(listener: &TcpListener) -> io::Result<()> {
    let addr = listener.local_addr()?;
    println!("{addr}");
    Ok(())
}

#[tracing::instrument(level = Level::INFO, skip_all, err)]
pub async fn proxy(config: LayerConfig, listen_port: u16, watch: drain::Watch) -> CliResult<()> {
    tracing::info!(
        ?config,
        listen_port,
        version = env!("CARGO_PKG_VERSION"),
        "Starting mirrord-extproxy",
    );

    let agent_connect_info: AgentConnectInfo = std::env::var_os(AGENT_CONNECT_INFO_ENV_KEY)
        .ok_or(ExternalProxyError::MissingConnectInfo)
        .and_then(|var| {
            serde_json::from_slice(var.as_bytes()).map_err(|error| {
                let as_string = String::from_utf8_lossy(var.as_bytes()).into_owned();
                ExternalProxyError::DeseralizeConnectInfo(as_string, error)
            })
        })?;

    let execution_kind = std::env::var(MIRRORD_EXECUTION_KIND_ENV)
        .ok()
        .and_then(|execution_kind| execution_kind.parse().ok())
        .unwrap_or_default();

    let mut analytics = AnalyticsReporter::new(config.telemetry, execution_kind, watch);
    (&config).collect_analytics(analytics.get_mut());

    // This connection is just to keep the agent alive as long as the client side is running.
    let mut own_agent_conn =
        connect_and_ping(&config, agent_connect_info.clone(), &mut analytics).await?;

    let tls_acceptor = match std::env::var_os(MIRRORD_EXTPROXY_TLS_SETUP_PEM) {
        Some(path) => mirrord_tls_util::SecureChannelSetup::create_acceptor(Path::new(&path))
            .await
            .map_err(ExternalProxyError::Tls)?
            .into(),
        None => None,
    };

    let ip_addr = config
        .external_proxy
        .host_ip
        .or_else(|| local_ip().ok())
        .unwrap_or_else(|| Ipv4Addr::UNSPECIFIED.into());

    let listener = create_listen_socket(SocketAddr::new(ip_addr, listen_port))
        .map_err(ExternalProxyError::ListenerSetup)?;
    print_addr(&listener).map_err(ExternalProxyError::ListenerSetup)?;

    if let Err(error) = unsafe { detach_io() }.map_err(ExternalProxyError::SetSid) {
        tracing::warn!(%error, "unable to detach io");
    }

    let cancellation_token = CancellationToken::new();
    let connections = Arc::new(AtomicUsize::new(0));
    let idle_timeout = config.external_proxy.idle_timeout;

    let mut initial_connection_timeout = Box::pin(tokio::time::sleep(Duration::from_secs(
        config.external_proxy.start_idle_timeout,
    )));

    let mut ping_pong_ticker = tokio::time::interval(Duration::from_secs(30));

    loop {
        tokio::select! {
            conn = listener.accept() => {
                if let Ok((stream, peer_addr)) = conn {
                    tracing::debug!(?peer_addr, "new connection");

                    let tls_acceptor = tls_acceptor.clone();
                    let connections = connections.clone();
                    let cancellation_token = cancellation_token.clone();
                    let connection_cancelation_token = cancellation_token.child_token();

                    let agent_conn = connect_and_ping(&config, agent_connect_info.clone(), &mut analytics).await.inspect_err(|_| cancellation_token.cancel())?;
                    connections.fetch_add(1, Ordering::Relaxed);

                    let fut = async move {
                        let stream = match tls_acceptor {
                            Some(tls_acceptor) => Either::Right(tls_acceptor.accept(stream).await?),
                            None => Either::Left(stream)
                        };

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

            message = own_agent_conn.agent_rx.recv() => {
                tracing::debug!(?message, "received message on own connection");

                match message {
                    Some(DaemonMessage::Pong) => continue,
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
                        return Err(
                            ExternalProxyError::PingPongFailed(format!(
                                "agent closed connection with message: {reason}"
                            )).into()
                        );
                    }
                    Some(message) => {
                        return Err(
                            ExternalProxyError::PingPongFailed(format!(
                                "agent sent an unexpected message: {message:?}"
                            )).into()
                        );
                    }
                    None => {
                        return Err(
                            ExternalProxyError::PingPongFailed(
                                "agent unexpectedly closed connection".to_string(),
                            ).into()
                        );
                    }
                }
            }

            _ = ping_pong_ticker.tick() => {
                tracing::debug!("sending ping");

                let _ = own_agent_conn.agent_tx.send(ClientMessage::Ping).await;
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
    stream: Either<TcpStream, TlsStream<TcpStream>>,
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
