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
    io::{ErrorKind, Write},
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

use futures::{stream::StreamExt, SinkExt};
use mirrord_analytics::{send_analytics, Analytics, CollectAnalytics};
use mirrord_config::LayerConfig;
use mirrord_kube::api::{kubernetes::KubernetesAPI, wrap_raw_connection, AgentManagment};
use mirrord_operator::client::{OperatorApi, OperatorSessionInformation};
use mirrord_protocol::{pause::DaemonPauseTarget, ClientMessage, DaemonCodec, DaemonMessage};
use nix::libc;
use tokio::{
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc,
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use tracing::{
    error, info,
    log::{debug, trace},
};

use crate::{
    config::InternalProxyArgs,
    error::{InternalProxyError, Result},
};

unsafe fn redirect_fd_to_dev_null(fd: libc::c_int) {
    let devnull_fd = libc::open(b"/dev/null\0" as *const [u8; 10] as _, libc::O_RDWR);
    libc::dup2(devnull_fd, fd);
    libc::close(devnull_fd);
}

unsafe fn detach_io() -> Result<()> {
    // Create a new session for the proxy process, detaching from the original terminal.
    // This makes the process not to receive signals from the "mirrord" process or it's parent
    // terminal fixes some side effects such as https://github.com/metalbear-co/mirrord/issues/1232
    nix::unistd::setsid().map_err(InternalProxyError::SetSidError)?;

    // flush before redirection
    {
        // best effort
        let _ = std::io::stdout().lock().flush();
    }
    for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO] {
        redirect_fd_to_dev_null(fd);
    }
    Ok(())
}

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
async fn connection_task(config: LayerConfig, stream: TcpStream) {
    debug!("intproxy connect_and_ping");
    let agent_connection = match timeout(Duration::from_secs(5), connect_and_ping(&config)).await {
        Ok(Ok((agent_connection, _))) => agent_connection,
        Ok(Err(err)) => {
            error!("connection to agent failed {err:#?}");
            return;
        }
        Err(timeout_error) => {
            error!("intproxy creation of connection to agent timed out: {timeout_error:?}");
            return;
        }
    };
    debug!("starting connection task, layer_stream: {stream:?}");
    let mut layer_connection = actix_codec::Framed::new(stream, DaemonCodec::new());
    let (agent_sender, mut agent_receiver) = agent_connection;
    loop {
        select! {
            layer_message = layer_connection.next() => {
                debug!("next layer to intproxy message: {layer_message:?}");
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
                debug!("next agent to intproxy message: {agent_message:?}");
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

/// Request target container pause from the connected agent.
async fn request_pause(
    sender: &mut mpsc::Sender<ClientMessage>,
    receiver: &mut mpsc::Receiver<DaemonMessage>,
) -> Result<(), InternalProxyError> {
    info!("Requesting target container pause from the agent");
    sender
        .send(ClientMessage::PauseTargetRequest(true))
        .await
        .map_err(|_| {
            InternalProxyError::PauseError("Failed to request target container pause.".to_string())
        })?;

    match receiver.recv().await {
        Some(DaemonMessage::PauseTarget(DaemonPauseTarget::PauseResponse {
            changed,
            container_paused: true,
        })) => {
            if changed {
                info!("Target container is now paused.");
            } else {
                info!("Target container was already paused.");
            }
            Ok(())
        }
        msg => Err(InternalProxyError::PauseError(format!(
            "Failed pausing, got invalid answer: {msg:#?}"
        ))),
    }
}

/// Creates a listening socket using socket2
/// to control the backlog and manage scenarios where
/// the proxy is under heavy load.
/// https://github.com/metalbear-co/mirrord/issues/1716#issuecomment-1663736500
/// in macOS backlog is documented to be hardcoded limited to 128.
fn create_listen_socket() -> Result<TcpListener, InternalProxyError> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )
    .map_err(InternalProxyError::ListenError)?;

    socket
        .bind(&socket2::SockAddr::from(SocketAddrV4::new(
            Ipv4Addr::LOCALHOST,
            0,
        )))
        .map_err(InternalProxyError::ListenError)?;
    socket
        .listen(1024)
        .map_err(InternalProxyError::ListenError)?;

    socket
        .set_nonblocking(true)
        .map_err(InternalProxyError::ListenError)?;

    // socket2 -> std -> tokio
    TcpListener::from_std(socket.into()).map_err(InternalProxyError::ListenError)
}

/// Main entry point for the internal proxy.
/// It listens for inbound layer connect and forwards to agent.
pub(crate) async fn proxy(args: InternalProxyArgs) -> Result<()> {
    let started = std::time::Instant::now();
    // Let it assign port for us then print it for the user.
    let listener = create_listen_socket()?;

    let config = LayerConfig::from_env()?;
    // Create a main connection, that will be held until proxy is closed.
    // This will guarantee agent staying alive and will enable us to
    // make the agent close on last connection close immediately (will help in tests)
    let (mut main_connection, operator) = connect_and_ping(&config).await?;
    if config.pause {
        tokio::time::timeout(
            Duration::from_secs(config.agent.communication_timeout.unwrap_or(30).into()),
            request_pause(&mut main_connection.0, &mut main_connection.1),
        )
        .await
        .map_err(|_| {
            InternalProxyError::PauseError(
                "Timeout requesting for target container pause.".to_string(),
            )
        })??;
    }

    let (main_connection_cancellation_token, main_connection_task_join) =
        create_ping_loop(main_connection);

    print_port(&listener)?;

    // wait for first connection `FIRST_CONNECTION_TIMEOUT` seconds, or timeout.
    let (stream, _) = timeout(Duration::from_secs(args.timeout), listener.accept())
        .await
        .map_err(|_| InternalProxyError::FirstConnectionTimeout)?
        .map_err(InternalProxyError::AcceptError)?;

    let mut active_connections = JoinSet::new();

    active_connections.spawn(connection_task(config.clone(), stream));

    unsafe {
        detach_io()?;
    }

    loop {
        tokio::select! {
            res = listener.accept() => {
                match res {
                    Ok((stream, _)) => {
                        let config = config.clone();
                        active_connections.spawn(connection_task(config, stream));
                    },
                    Err(err) => {
                        error!("Error accepting connection: {err:#?}");
                        break;
                    }
                }
            },
            _ = active_connections.join_next(), if !active_connections.is_empty() => {
                debug!("connection_task (intproxy <--> layer) completed.")
            },
            _ = main_connection_cancellation_token.cancelled() => {
                debug!("intproxy main connection canceled.");
                break;
            }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                if active_connections.is_empty() {
                    debug!("intproxy timeout, no active connections. Exiting.");
                    break;
                }
                debug!("intproxy 5 sec tick, active_connections: {active_connections:?}.");
            }
        }
    }
    main_connection_cancellation_token.cancel();

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

    debug!("intproxy joining main connection task");
    match main_connection_task_join.await {
        Ok(Err(err)) => Err(err.into()),
        Err(err) => {
            error!("internal_proxy connection paniced {err}");

            Err(InternalProxyError::AgentClosedConnection.into())
        }
        _ => Ok(()),
    }
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

fn create_ping_loop(
    mut connection: (mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>),
) -> (
    CancellationToken,
    JoinHandle<Result<(), InternalProxyError>>,
) {
    let cancellation_token = CancellationToken::new();

    let join_handle = tokio::spawn({
        let cancellation_token = cancellation_token.clone();

        async move {
            let mut main_keep_interval = tokio::time::interval(Duration::from_secs(30));
            main_keep_interval.tick().await;

            loop {
                tokio::select! {
                    _ = main_keep_interval.tick() => {
                        if let Err(err) = ping(&mut connection.0, &mut connection.1).await {
                            cancellation_token.cancel();

                            return Err(err);
                        }
                    }
                    _ = cancellation_token.cancelled() => {
                        break;
                    }
                }
            }

            Ok(())
        }
    });

    (cancellation_token, join_handle)
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
    if let Some(operator_session_information) = OperatorSessionInformation::from_env()? {
        Ok((
            OperatorApi::connect(config, &operator_session_information).await?,
            true,
        ))
    } else if let Some(address) = &config.connect_tcp {
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
        Err(InternalProxyError::NoConnectionMethod.into())
    }
}
