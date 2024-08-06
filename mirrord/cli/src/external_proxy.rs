//!
//! External proxy to pipe communication from intproxy to agent
//!
//! ```
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
    fs::{File, OpenOptions},
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
use mirrord_protocol::DaemonCodec;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::server::TlsStream;
use tokio_util::{codec::Framed, sync::CancellationToken};
use tracing::Level;
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

    let agent_connect_info = std::env::var(AGENT_CONNECT_INFO_ENV_KEY).ok().map(|var| {
    serde_json::from_str(&var)
                .map_err(|e| ExternalProxyError::DeseralizeConnectInfo(var, e))
    }).transpose()?;
    let mut analytics = AnalyticsReporter::new(config.telemetry, watch);
    (&config).collect_analytics(analytics.get_mut());

    let tls_acceptor = create_extenral_proxy_tls_acceptor(&config).await?;
    let listener = create_listen_socket(SocketAddr::new(
        local_ip().unwrap_or_else(|_| Ipv4Addr::UNSPECIFIED.into()),
        0,
    ))
    .map_err(ExternalProxyError::ListenerSetup)?;
    print_addr(&listener).map_err(ExternalProxyError::ListenerSetup)?;

    unsafe { detach_io() }.map_err(ExternalProxyError::SetSid)?;

    let cancellation_token = CancellationToken::new();
    let connections = Arc::new(AtomicUsize::new(0));

    let mut initial_connection_timeout = Box::pin(tokio::time::sleep(Duration::from_secs(
        config.external_proxy.start_idle_timeout,
    )));

    loop {
        tokio::select! {
            conn = listener.accept() => {
                if let Ok((stream, peer_addr)) = conn {

                    tracing::debug!(?peer_addr, "new connection");

                    let tls_acceptor = tls_acceptor.clone();
                    let connections = connections.clone();
                    let cancellation_token = cancellation_token.clone();

                    let agent_conn =
                        connect_and_ping(&config, agent_connect_info.clone(), &mut analytics).await?;


                    let fut = async move {
                        let stream = tls_acceptor.accept(stream).await?;

                        connections.fetch_add(1, Ordering::Relaxed);

                        handle_connection(stream, agent_conn).await;

                        tracing::debug!(?peer_addr, "closed connection");

                        if connections.fetch_sub(1, Ordering::Relaxed) == 1 {
                            cancellation_token.cancel();

                            tracing::debug!(?peer_addr, "final connection, closing listener");
                        }

                        Ok::<(), std::io::Error>(())
                    };

                    tokio::spawn(async move {
                        if let Err(error) = fut.await {
                            tracing::error!(%error, "error handling proxy connection");
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

async fn create_extenral_proxy_tls_acceptor(
    config: &LayerConfig,
) -> Result<tokio_rustls::TlsAcceptor, ExternalProxyError> {
    let (Some(client_tls_certificate), Some(tls_certificate), Some(tls_key)) = (
        config.external_proxy.client_tls_certificate.as_ref(),
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
    .collect::<Result<Vec<_>, _>>()
    .map_err(|error| ConnectionTlsError::ParsingPem(client_tls_certificate.to_path_buf(), error))
    .map_err(ExternalProxyError::Tls)?;

    let tls_certificate = rustls_pemfile::certs(&mut BufReader::new(
        File::open(tls_certificate)
            .map_err(|error| ConnectionTlsError::MissingPem(tls_certificate.to_path_buf(), error))
            .map_err(ExternalProxyError::Tls)?,
    ))
    .collect::<Result<Vec<_>, _>>()
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

    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(tls_config)))
}

#[tracing::instrument(level = Level::TRACE, skip(agent_conn))]
async fn handle_connection(stream: TlsStream<TcpStream>, mut agent_conn: AgentConnection) {
    let mut stream = Framed::new(stream, DaemonCodec::default());

    loop {
        tokio::select! {
            client_message = stream.next() => {
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
                    if let Err(error) = stream.send(daemon_message).await {
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
