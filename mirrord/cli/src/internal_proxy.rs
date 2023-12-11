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
    env,
    io::Write,
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

use mirrord_analytics::{AnalyticsError, AnalyticsReporter, CollectAnalytics};
use mirrord_config::LayerConfig;
use mirrord_intproxy::{
    agent_conn::{AgentConnectInfo, AgentConnection},
    IntProxy,
};
use mirrord_protocol::{pause::DaemonPauseTarget, ClientCodec, ClientMessage, DaemonMessageV1};
use nix::libc;
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, log::trace};

use crate::{
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    error::{CliError, InternalProxySetupError, Result},
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
    nix::unistd::setsid().map_err(InternalProxySetupError::SetSidError)?;

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
        .map_err(InternalProxySetupError::LocalPortError)?
        .port();
    println!("{port}\n");
    Ok(())
}

/// Request target container pause from the connected agent.
async fn request_pause(
    sender: &mpsc::Sender<ClientMessage>,
    receiver: &mut mpsc::Receiver<DaemonMessageV1>,
) -> Result<(), InternalProxySetupError> {
    info!("Requesting target container pause from the agent");
    sender
        .send(ClientMessage::PauseTargetRequest(true))
        .await
        .map_err(|_| {
            InternalProxySetupError::PauseError(
                "Failed to request target container pause.".to_string(),
            )
        })?;

    match receiver.recv().await {
        Some(DaemonMessageV1::PauseTarget(DaemonPauseTarget::PauseResponse {
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
        msg => Err(InternalProxySetupError::PauseError(format!(
            "Failed pausing, got invalid answer: {msg:#?}"
        ))),
    }
}

/// Creates a listening socket using socket2
/// to control the backlog and manage scenarios where
/// the proxy is under heavy load.
/// https://github.com/metalbear-co/mirrord/issues/1716#issuecomment-1663736500
/// in macOS backlog is documented to be hardcoded limited to 128.
fn create_listen_socket() -> Result<TcpListener, InternalProxySetupError> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )
    .map_err(InternalProxySetupError::ListenError)?;

    socket
        .bind(&socket2::SockAddr::from(SocketAddrV4::new(
            Ipv4Addr::LOCALHOST,
            0,
        )))
        .map_err(InternalProxySetupError::ListenError)?;
    socket
        .listen(1024)
        .map_err(InternalProxySetupError::ListenError)?;

    socket
        .set_nonblocking(true)
        .map_err(InternalProxySetupError::ListenError)?;

    // socket2 -> std -> tokio
    TcpListener::from_std(socket.into()).map_err(InternalProxySetupError::ListenError)
}

fn get_agent_connect_info() -> Result<Option<AgentConnectInfo>> {
    let Ok(var) = env::var(AGENT_CONNECT_INFO_ENV_KEY) else {
        return Ok(None);
    };

    serde_json::from_str(&var).map_err(|e| CliError::ConnectInfoLoadFailed(var, e))
}

/// Main entry point for the internal proxy.
/// It listens for inbound layer connect and forwards to agent.
pub(crate) async fn proxy(watch: drain::Watch) -> Result<()> {
    let config = LayerConfig::from_env()?;
    let agent_connect_info = get_agent_connect_info()?;

    let mut analytics = AnalyticsReporter::new(config.telemetry, watch);
    (&config).collect_analytics(analytics.get_mut());

    // Let it assign port for us then print it for the user.
    let listener = create_listen_socket()?;

    // Create a main connection, that will be held until proxy is closed.
    // This will guarantee agent staying alive and will enable us to
    // make the agent close on last connection close immediately (will help in tests)
    let mut main_connection = connect_and_ping(&config, agent_connect_info.clone(), &mut analytics)
        .await
        .inspect_err(|_| analytics.set_error(AnalyticsError::AgentConnection))?;

    if config.pause {
        tokio::time::timeout(
            Duration::from_secs(config.agent.communication_timeout.unwrap_or(30).into()),
            request_pause(&main_connection.0, &mut main_connection.1),
        )
        .await
        .map_err(|_| {
            InternalProxySetupError::PauseError(
                "Timeout requesting for target container pause.".to_string(),
            )
        })??;
    }

    let (main_connection_cancellation_token, main_connection_task_join) =
        create_ping_loop(main_connection);

    print_port(&listener)?;

    unsafe {
        detach_io()?;
    }

    let first_connection_timeout = Duration::from_secs(config.internal_proxy.start_idle_timeout);
    let consecutive_connection_timeout = Duration::from_secs(config.internal_proxy.idle_timeout);

    // TODO: don't unwrap.
    let (mut codec, _version) = mirrord_protover::determine_version(stream).await.unwrap();

    IntProxy::new(&config, agent_connect_info, listener)
        .await?
        .run(first_connection_timeout, consecutive_connection_timeout)
        .await?;

    main_connection_cancellation_token.cancel();

    trace!("intproxy joining main connection task");
    match main_connection_task_join.await {
        Ok(Err(err)) => Err(err.into()),
        Err(err) => {
            error!("internal_proxy connection panicked {err}");

            Err(InternalProxySetupError::AgentClosedConnection.into())
        }
        _ => Ok(()),
    }
}

/// Connect and send ping - this is useful when working using k8s
/// port forward since it only creates the connection after
/// sending the first message
async fn connect_and_ping<I, O>(
    config: &LayerConfig,
    agent_connect_info: Option<AgentConnectInfo>,
    analytics: &mut AnalyticsReporter,
) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessageV1>)> {
    let AgentConnection {
        agent_tx,
        mut agent_rx,
    } = AgentConnection::new(config, agent_connect_info, Some(analytics)).await?;
    ping(&agent_tx, &mut agent_rx).await?;
    Ok((agent_tx, agent_rx))
}

/// Sends a ping the connection and expects a pong.
async fn ping(
    sender: &mpsc::Sender<ClientMessage>,
    receiver: &mut mpsc::Receiver<DaemonMessageV1>,
) -> Result<(), InternalProxySetupError> {
    sender.send(ClientMessage::Ping).await?;
    match receiver.recv().await {
        Some(DaemonMessageV1::Pong) => Ok(()),
        _ => Err(InternalProxySetupError::AgentClosedConnection),
    }
}

fn create_ping_loop(
    mut connection: (mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessageV1>),
) -> (
    CancellationToken,
    JoinHandle<Result<(), InternalProxySetupError>>,
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
                        if let Err(err) = ping(&connection.0, &mut connection.1).await {
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
