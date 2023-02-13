//! Internal proxy is accepting connection from local layers and forward it to agent
//! while having 1:1 relationship - each layer connection is another agent connection.
//! This might be changed later on.
//! The main advantage of this design is that we remove kube logic from the layer itself,
//! thus eliminating bugs that happen due to mix of remote env vars in our code
//! (previously was solved using envguard which wasn't good enough)
//! The proxy will either directly connect to an existing agent (currently only used for tests),
//! or let the [`OperatorApi`] handle the connection.

use std::{
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

use futures::{stream::StreamExt, SinkExt};
use mirrord_config::LayerConfig;
use mirrord_kube::{
    api::{kubernetes::KubernetesAPI, wrap_raw_connection, AgentManagment, Connection},
    error::KubeApiError,
};
use mirrord_operator::client::{OperatorApi, OperatorApiError};
use mirrord_progress::NoProgress;
use mirrord_protocol::{ClientMessage, DaemonCodec, DaemonMessage};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self, Receiver, Sender},
    task::JoinSet,
    time::timeout,
};
use tracing::log::info;

use crate::error::{InternalProxyError, Result};

const FIRST_CONNECTION_TIMEOUT: u64 = 5;

fn print_port(listener: &TcpListener) -> Result<()> {
    let port = listener
        .local_addr()
        .map_err(|e| InternalProxyError::LocalPortError(e))?
        .port();
    println!("{port:?}\n");
    Ok(())
}

/// Supposed to run as an async detached task, proxying the connection.
/// We parse the protocol so we might add some logic here in the future?
async fn handle_connection(
    stream: TcpStream,
    agent_connection: (mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>),
) {
    let mut layer_connection = actix_codec::Framed::new(stream, DaemonCodec::new());
    let (mut agent_sender, mut agent_receiver) = agent_connection;
    loop {
        select! {
            Some(layer_message) = layer_connection.next() => {
                agent_sender.send(layer_message.expect("invalid layer message")).await;
            },
            Some(agent_message) = agent_receiver.recv() => {
                layer_connection.send(agent_message).await;
            }
            else => {
                break;
            }
        }
    }
}

pub(crate) async fn proxy() -> Result<()> {
    // Let it assign port for us then print it for the user.
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|e| InternalProxyError::ListenError(e))?;

    let config = LayerConfig::from_env()?;
    // Create a main connection, that will be held until proxy is closed.
    // This will guarantee agent staying alive and will enable us to
    // make the agent close on last connection close immediately (will help in tests)
    let main_connection = connect(&config).await?;
    print_port(&listener)?;

    // wait for first connection `FIRST_CONNECTION_TIMEOUT` seconds, or timeout.
    let (stream, _) = timeout(
        Duration::from_secs(FIRST_CONNECTION_TIMEOUT),
        listener.accept(),
    )
    .await
    .map_err(|_| InternalProxyError::FirstConnectionTimeout)?
    .map_err(|e| InternalProxyError::AcceptError(e))?;

    let mut active_connections = JoinSet::new();

    let agent_connection = connect(&config).await?;
    active_connections.spawn(handle_connection(stream, agent_connection));

    loop {
        tokio::select! {
            Ok((stream, _)) = listener.accept() => {
                let agent_connection = connect(&config).await?;
                active_connections.spawn(handle_connection(stream, agent_connection));
            },
            _ = active_connections.join_next() => {},
            _ = tokio::time::sleep(Duration::from_secs(2)) => {
                if active_connections.is_empty() {
                    break;
                }
            }
        }
    }
    Ok(())
}

async fn connect(
    config: &LayerConfig,
) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
    if let Some(address) = &config.connect_tcp {
        let stream = TcpStream::connect(address)
            .await
            .map_err(InternalProxyError::TcpConnectError)?;
        Ok(wrap_raw_connection(stream))
    } else if let (Some(agent_name), Some(port)) =
        (&config.connect_agent_name, config.connect_agent_port)
    {
        let k8s_api = KubernetesAPI::create(config).await?;
        let connection = k8s_api.create_connection((agent_name.clone(), port)).await?;
        Ok(connection)
    } else {
        let connection = OperatorApi::discover(&config).await?;
        Ok(connection.ok_or(InternalProxyError::OperatorConnectionError)?)
    }
}
