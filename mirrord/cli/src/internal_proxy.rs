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
//! or let the [`OperatorApi`] handle the connection.

use std::{
    io::ErrorKind,
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

use futures::{stream::StreamExt, SinkExt};
use mirrord_analytics::{send_analytics, Analytics, CollectAnalytics};
use mirrord_config::LayerConfig;
use mirrord_kube::api::{kubernetes::KubernetesAPI, wrap_raw_connection, AgentManagment};
use mirrord_operator::client::OperatorApi;
use mirrord_progress::NoProgress;
use mirrord_protocol::{ClientMessage, DaemonCodec, DaemonMessage};
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{self},
    task::JoinSet,
    time::timeout,
};
use tracing::{error, log::trace};

use crate::{
    config::InternalProxyArgs,
    error::{InternalProxyError, Result},
};

/// Print the port for the caller (mirrord cli execution flow) so it can pass it
/// back to the layer instances via env var.
fn print_port(listener: &TcpListener) -> Result<()> {
    let port = listener
        .local_addr()
        .map_err(InternalProxyError::LocalPortError)?
        .port();
    println!("{port}\n");
    Ok(())
}

/// Supposed to run as an async detached task, proxying the connection.
/// We parse the protocol so we might add some logic here in the future?
async fn connection_task(
    stream: TcpStream,
    agent_connection: (mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>),
) {
    let mut layer_connection = actix_codec::Framed::new(stream, DaemonCodec::new());
    let (agent_sender, mut agent_receiver) = agent_connection;
    loop {
        select! {
            layer_message = layer_connection.next() => {
                match layer_message {
                    Some(Ok(layer_message)) => {
                        if let Err(err) = agent_sender.send(layer_message).await {
                            trace!("Error sending layer message to agent: {err:#?}");
                            break;
                        }
                    },
                    Some(Err(ref error)) if error.kind() == ErrorKind::ConnectionReset => {
                        trace!("layer connection reset");
                        break;
                    },
                    Some(Err(fail)) => {
                        error!("Error receiving layer message: {fail:#?}");
                        break;
                    },
                    None => {
                        trace!("layer connection closed");
                        break;
                    }
                }
            },
            agent_message = agent_receiver.recv() => {
                match agent_message {
                    Some(agent_message) => {
                        if let Err(err) = layer_connection.send(agent_message).await {
                            trace!("Error sending agent message to layer: {err:#?}");
                            break;
                        }
                    },
                    None => {
                        trace!("agent connection closed");
                        break;
                    }
                }
            }
        }
    }
}

/// Main entry point for the internal proxy.
/// It listens for inbound layer connect and forwards to agent.
pub(crate) async fn proxy(args: InternalProxyArgs) -> Result<()> {
    // Create a new session for the proxy process, detaching from the original terminal.
    // This makes the process not to receive signals from the "mirrord" process or it's parent
    // terminal fixes some side effects such as https://github.com/metalbear-co/mirrord/issues/1232
    nix::unistd::setsid().map_err(InternalProxyError::SetSidError)?;
    let started = std::time::Instant::now();
    // Let it assign port for us then print it for the user.
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(InternalProxyError::ListenError)?;

    let config = LayerConfig::from_env()?;
    // Create a main connection, that will be held until proxy is closed.
    // This will guarantee agent staying alive and will enable us to
    // make the agent close on last connection close immediately (will help in tests)
    let (_main_connection, operator) = connect_and_ping(&config).await?;
    print_port(&listener)?;

    // wait for first connection `FIRST_CONNECTION_TIMEOUT` seconds, or timeout.
    let (stream, _) = timeout(Duration::from_secs(args.timeout), listener.accept())
        .await
        .map_err(|_| InternalProxyError::FirstConnectionTimeout)?
        .map_err(InternalProxyError::AcceptError)?;

    let mut active_connections = JoinSet::new();

    let (agent_connection, _) = connect_and_ping(&config).await?;
    active_connections.spawn(connection_task(stream, agent_connection));

    loop {
        tokio::select! {
            res = listener.accept() => {
                match res {
                    Ok((stream, _)) => {
                        let (agent_connection, _) = connect_and_ping(&config).await?;
                        active_connections.spawn(connection_task(stream, agent_connection));
                    },
                    Err(err) => {
                        error!("Error accepting connection: {err:#?}");
                        break;
                    }
                }
            },
            _ = active_connections.join_next(), if !active_connections.is_empty() => {},
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                if active_connections.is_empty() {
                    break;
                }
            }
        }
    }
    let mut analytics = Analytics::default();
    (&config).collect_analytics(&mut analytics);
    if config.telemetry {
        send_analytics(
            analytics,
            started.elapsed().as_secs().try_into().unwrap_or(u32::MAX),
            operator,
        )
        .await;
    }
    Ok(())
}

/// Connect and send ping - this is useful when working using k8s
/// port forward since it only creates the connection after
/// sending the first message
async fn connect_and_ping(
    config: &LayerConfig,
) -> Result<(
    (mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>),
    bool,
)> {
    let ((mut sender, mut receiver), operator) = connect(config).await?;
    ping(&mut sender, &mut receiver).await?;
    Ok(((sender, receiver), operator))
}

/// Sends a ping the connection and expects a pong.
async fn ping(
    sender: &mut mpsc::Sender<ClientMessage>,
    receiver: &mut mpsc::Receiver<DaemonMessage>,
) -> Result<(), InternalProxyError> {
    sender.send(ClientMessage::Ping).await?;
    match receiver.recv().await {
        Some(DaemonMessage::Pong) => Ok(()),
        _ => Err(InternalProxyError::AgentClosedConnection),
    }
}

/// Connects to an agent pod depending on how [`LayerConfig`] is set-up:
///
/// - `connect_tcp`: connects directly to the `address` specified, and calls [`wrap_raw_connection`]
///   on the [`TcpStream`];
///
/// - `connect_agent_name`: Connects to an agent with `connect_agent_name` on `connect_agent_port`
///   using [`KubernetesAPI];
///
/// - None of the above: uses the [`OperatorApi`] to establish the connection.
/// Returns the tx/rx and whether the operator is used.
async fn connect(
    config: &LayerConfig,
) -> Result<(
    (mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>),
    bool,
)> {
    if let Some(address) = &config.connect_tcp {
        let stream = TcpStream::connect(address)
            .await
            .map_err(InternalProxyError::TcpConnectError)?;
        Ok((wrap_raw_connection(stream), false))
    } else if let (Some(agent_name), Some(port)) =
        (&config.connect_agent_name, config.connect_agent_port)
    {
        let k8s_api = KubernetesAPI::create(config).await?;
        let connection = k8s_api
            .create_connection((agent_name.clone(), port))
            .await?;
        Ok((connection, false))
    } else {
        let connection = OperatorApi::discover(config, &NoProgress).await?;
        Ok((
            connection.ok_or(InternalProxyError::OperatorConnectionError)?,
            true,
        ))
    }
}
