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
    fs::File,
    io,
    io::BufReader,
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
use mirrord_intproxy::agent_conn::{AgentConnection, ConnectionTlsError};
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
    logging::init_extproxy_tracing_registry,
    util::{create_listen_socket, detach_io},
};

/// Print the address for the caller (mirrord cli execution flow) so it can pass it
/// back to the layer instances via env var.
fn print_addr(listener: &TcpListener) -> io::Result<()> {
    let addr = listener.local_addr()?;
    println!("{addr}\n");
    Ok(())
}

pub async fn proxy(listen_port: u16, watch: drain::Watch) -> CliResult<()> {
    let config = LayerConfig::recalculate_from_env()?;

    init_extproxy_tracing_registry(&config)?;
    tracing::info!(?config, "external_proxy starting");

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

    let tls_acceptor = create_external_proxy_tls_acceptor(&config).await?;
    let listener = create_listen_socket(SocketAddr::new(
        local_ip().unwrap_or_else(|_| Ipv4Addr::UNSPECIFIED.into()),
        listen_port,
    ))
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

    // This connection is just to keep the agent alive as long as the client side is running.
    let mut own_agent_conn = connect_and_ping(&config, agent_connect_info.clone(), &mut analytics)
        .await
        .inspect_err(|_| cancellation_token.cancel())?;

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

async fn create_external_proxy_tls_acceptor(
    config: &LayerConfig,
) -> CliResult<Option<tokio_rustls::TlsAcceptor>, ExternalProxyError> {
    if !config.external_proxy.tls_enable {
        return Ok(None);
    }

    let (Some(client_tls_certificate), Some(tls_certificate), Some(tls_key)) = (
        config.internal_proxy.client_tls_certificate.as_ref(),
        config.external_proxy.tls_certificate.as_ref(),
        config.external_proxy.tls_key.as_ref(),
    ) else {
        return Err(ExternalProxyError::MissingTlsInfo);
    };

    let tls_client_certificates = rustls_pemfile::certs(&mut BufReader::new(
        File::open(client_tls_certificate)
            .map_err(|error| {
                ConnectionTlsError::MissingPem(client_tls_certificate.to_path_buf(), error)
            })
            .map_err(ExternalProxyError::Tls)?,
    ))
    .collect::<CliResult<Vec<_>, _>>()
    .map_err(|error| ConnectionTlsError::ParsingPem(client_tls_certificate.to_path_buf(), error))
    .map_err(ExternalProxyError::Tls)?;

    let tls_certificate = rustls_pemfile::certs(&mut BufReader::new(
        File::open(tls_certificate)
            .map_err(|error| ConnectionTlsError::MissingPem(tls_certificate.to_path_buf(), error))
            .map_err(ExternalProxyError::Tls)?,
    ))
    .collect::<CliResult<Vec<_>, _>>()
    .map_err(|error| ConnectionTlsError::ParsingPem(tls_certificate.to_path_buf(), error))
    .map_err(ExternalProxyError::Tls)?;

    let tls_keys = rustls_pemfile::private_key(&mut BufReader::new(
        File::open(tls_key)
            .map_err(|error| ConnectionTlsError::MissingPem(tls_key.to_path_buf(), error))?,
    ))
    .map_err(|error| ConnectionTlsError::ParsingPem(tls_key.to_path_buf(), error))?
    .ok_or_else(|| ConnectionTlsError::MissingPrivateKey(tls_key.to_path_buf()))?;

    let mut roots = rustls::RootCertStore::empty();

    roots.add_parsable_certificates(tls_client_certificates);

    let client_verifier = rustls::server::WebPkiClientVerifier::builder(roots.into())
        .build()
        .map_err(ConnectionTlsError::ClientVerifier)?;

    let tls_config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(tls_certificate, tls_keys)
        .map_err(ConnectionTlsError::ServerConfig)?;

    Ok(Some(tokio_rustls::TlsAcceptor::from(Arc::new(tls_config))))
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
