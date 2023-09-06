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
use mirrord_analytics::{AnalyticsError, AnalyticsReporter, CollectAnalytics};
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
use tracing::{error, info, log::trace, warn};

use crate::{
    connection::AgentConnectInfo,
    error::{InternalProxyError, Result},
};

const PING_INTERVAL_DURATION: Duration = Duration::from_secs(30);

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
    for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
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
/// We also handle pings here, meaning that if layer is too quiet (for example if it has a
/// breakpoint hit and someone is holding it) It will keep the agent alive and send pings on its
/// behalf.
async fn connection_task(config: LayerConfig, stream: TcpStream) {
    let mut inactive_analytics = AnalyticsReporter::new(false);

    let Ok(agent_connection) = connect_and_ping(&config, &mut inactive_analytics)
        .await
        .inspect_err(|err| error!("connection to agent failed {err:#?}")) else { return; };

    let mut layer_connection = actix_codec::Framed::new(stream, DaemonCodec::new());
    let (agent_sender, mut agent_receiver) = agent_connection;
    let mut ping = false;
    let mut ping_interval = tokio::time::interval(PING_INTERVAL_DURATION);
    ping_interval.tick().await;
    loop {
        select! {
            layer_message = layer_connection.next() => {
                ping_interval.reset();
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
                    Some(DaemonMessage::Pong) => {
                        ping = false;
                    },
                    Some(agent_message) => {
                        if let Err(err) = layer_connection.send(agent_message).await {
                            trace!("Error sending agent message to layer: {err:#?}");
                            break;
                        }
                    }
                    None => {
                        trace!("agent connection closed");
                        break;
                    }
                }
            },
            _ = ping_interval.tick() => {
                if !ping {
                    if let Err(err) = agent_sender.send(ClientMessage::Ping).await {
                        trace!("Error sending ping to agent: {err:#?}");
                        break;
                    }
                    ping = true;
                } else {
                    warn!("Unmatched ping, timeout!");
                    break;
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
pub(crate) async fn proxy(watch: drain::Watch) -> Result<()> {
    let config = LayerConfig::from_env()?;

    let mut analytics = AnalyticsReporter::new(config.telemetry, watch);
    (&config).collect_analytics(analytics.get_mut());

    // Let it assign port for us then print it for the user.
    let listener = create_listen_socket()?;

    // Create a main connection, that will be held until proxy is closed.
    // This will guarantee agent staying alive and will enable us to
    // make the agent close on last connection close immediately (will help in tests)
    let mut main_connection = connect_and_ping(&config, &mut analytics)
        .await
        .inspect_err(|_| analytics.set_error(AnalyticsError::AgentConnection))?;

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

    let (stream, _) = timeout(
        Duration::from_secs(config.internal_proxy.start_idle_timeout),
        listener.accept(),
    )
    .await
    .map_err(|_| {
        analytics.set_error(AnalyticsError::IntProxyFirstConnection);

        InternalProxyError::FirstConnectionTimeout
    })?
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
            _ = active_connections.join_next(), if !active_connections.is_empty() => {},
            _ = main_connection_cancellation_token.cancelled() => {
                trace!("intproxy main connection canceled.");
                break;
            }
            _ = tokio::time::sleep(Duration::from_secs(config.internal_proxy.idle_timeout)) => {
                if active_connections.is_empty() {
                    trace!("intproxy timeout, no active connections. Exiting.");
                    break;
                }
                trace!("intproxy {} sec tick, active_connections: {active_connections:?}.", config.internal_proxy.idle_timeout);
            }
        }
    }
    main_connection_cancellation_token.cancel();

    trace!("intproxy joining main connection task");
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
    analytics: &mut AnalyticsReporter,
) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>)> {
    let ((mut sender, mut receiver), _) = connect(config, analytics).await?;
    ping(&mut sender, &mut receiver).await?;
    Ok((sender, receiver))
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
    analytics: &mut AnalyticsReporter,
) -> Result<(
    (mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>),
    Option<OperatorSessionInformation>,
)> {
    let agent_connect_info = AgentConnectInfo::from_env()?;
    match agent_connect_info {
        Some(AgentConnectInfo::Operator(operator_session_information)) => Ok((
            OperatorApi::connect(config, &operator_session_information, analytics).await?,
            Some(operator_session_information),
        )),
        Some(AgentConnectInfo::DirectKubernetes(connect_info)) => {
            let k8s_api = KubernetesAPI::create(config).await?;
            let connection = k8s_api.create_connection(connect_info).await?;
            Ok((connection, None))
        }
        None => {
            if let Some(address) = &config.connect_tcp {
                let stream = TcpStream::connect(address)
                    .await
                    .map_err(InternalProxyError::TcpConnectError)?;
                Ok((wrap_raw_connection(stream), None))
            } else {
                Err(InternalProxyError::NoConnectionMethod.into())
            }
        }
    }
}
