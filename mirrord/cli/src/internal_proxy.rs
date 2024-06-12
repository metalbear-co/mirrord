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
//! or let the [`OperatorApi`](mirrord_operator::client::OperatorApi) handle the connection.

use std::{
    env,
    fs::OpenOptions,
    io::{self, Write},
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

use mirrord_analytics::{AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::LayerConfig;
use mirrord_intproxy::IntProxy;
use nix::{
    libc,
    sys::resource::{setrlimit, Resource},
};
use tokio::net::TcpListener;
use tracing::warn;
use tracing_subscriber::EnvFilter;

use crate::{
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    error::{InternalProxyError, Result},
};

unsafe fn redirect_fd_to_dev_null(fd: libc::c_int) {
    let devnull_fd = libc::open(b"/dev/null\0" as *const [u8; 10] as _, libc::O_RDWR);
    libc::dup2(devnull_fd, fd);
    libc::close(devnull_fd);
}

unsafe fn detach_io() -> Result<(), InternalProxyError> {
    // Create a new session for the proxy process, detaching from the original terminal.
    // This makes the process not to receive signals from the "mirrord" process or it's parent
    // terminal fixes some side effects such as https://github.com/metalbear-co/mirrord/issues/1232
    nix::unistd::setsid().map_err(InternalProxyError::SetSid)?;

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
fn print_port(listener: &TcpListener) -> io::Result<()> {
    let port = listener.local_addr()?.port();
    println!("{port}\n");
    Ok(())
}

/// Creates a listening socket using socket2
/// to control the backlog and manage scenarios where
/// the proxy is under heavy load.
/// <https://github.com/metalbear-co/mirrord/issues/1716#issuecomment-1663736500>
/// in macOS backlog is documented to be hardcoded limited to 128.
fn create_listen_socket() -> io::Result<TcpListener> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;

    socket.bind(&socket2::SockAddr::from(SocketAddrV4::new(
        Ipv4Addr::LOCALHOST,
        0,
    )))?;
    socket.listen(1024)?;
    socket.set_nonblocking(true)?;

    // socket2 -> std -> tokio
    TcpListener::from_std(socket.into())
}

/// Main entry point for the internal proxy.
/// It listens for inbound layer connect and forwards to agent.
pub(crate) async fn proxy(watch: drain::Watch) -> Result<(), InternalProxyError> {
    let config = LayerConfig::from_env()?;

    if let Some(log_destination) = config.internal_proxy.log_destination.as_ref() {
        let output_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_destination)
            .map_err(|e| InternalProxyError::OpenLogFile(log_destination.clone(), e))?;

        let tracing_registry = tracing_subscriber::fmt()
            .with_writer(output_file)
            .with_ansi(false);

        if let Some(log_level) = config.internal_proxy.log_level.as_ref() {
            tracing_registry
                .with_env_filter(EnvFilter::builder().parse_lossy(log_level))
                .init();
        } else {
            tracing_registry.init();
        }
    }

    // According to https://wilsonmar.github.io/maximum-limits/ this is the limit on macOS
    // so we assume Linux can be higher and set to that.
    if let Err(error) = setrlimit(Resource::RLIMIT_NOFILE, 12288, 12288) {
        warn!(?error, "Failed to set the file descriptor limit");
    }

    let agent_connect_info = match env::var(AGENT_CONNECT_INFO_ENV_KEY) {
        Ok(var) => {
            let deserialized = serde_json::from_str(&var)
                .map_err(|e| InternalProxyError::DeseralizeConnectInfo(var, e))?;
            Some(deserialized)
        }
        Err(..) => None,
    };

    let mut analytics = AnalyticsReporter::new(config.telemetry, watch);
    (&config).collect_analytics(analytics.get_mut());

    // Let it assign port for us then print it for the user.
    let listener = create_listen_socket().map_err(InternalProxyError::ListenerSetup)?;
    print_port(&listener).map_err(InternalProxyError::ListenerSetup)?;

    unsafe {
        detach_io()?;
    }

    let first_connection_timeout = Duration::from_secs(config.internal_proxy.start_idle_timeout);
    let consecutive_connection_timeout = Duration::from_secs(config.internal_proxy.idle_timeout);

    IntProxy::new(&config, agent_connect_info, listener)
        .await?
        .run(first_connection_timeout, consecutive_connection_timeout)
        .await?;

    Ok(())
}
