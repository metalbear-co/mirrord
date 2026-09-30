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

pub(crate) mod db_portforwards;

#[cfg(not(target_os = "windows"))]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "macos")]
use std::sync::atomic::Ordering;
use std::{
    env::{self, home_dir},
    io,
    net::{Ipv4Addr, SocketAddr},
    ops::Not,
    sync::{Arc, Weak, atomic::AtomicU32},
    time::Duration,
};

use mirrord_analytics::{
    AnalyticsError, AnalyticsReporter, CollectAnalytics, Reporter, read_correlation_id_from_env,
    read_kube_version_from_env,
};
use mirrord_config::{
    LayerConfig, LayerFileConfig,
    config::{ConfigContext, MirrordConfig},
};
use mirrord_intproxy::{
    IntProxy, IntProxyIntervals,
    agent_conn::{AgentConnectInfo, AgentConnection},
    session_monitor::{
        MonitorTx,
        chaos::{ChaosWatcherRx, ChaosWatcherTx, analytics::ChaosAnalyticsReporter},
    },
};
use mirrord_protocol::{ClientMessage, DaemonMessage, LogLevel, LogMessage};
use mirrord_session_monitor_protocol::SessionInfo;
#[cfg(not(target_os = "windows"))]
use nix::sys::resource::{Resource, setrlimit};
#[cfg(unix)]
use nix::{
    sys::signal::{Signal, kill},
    unistd::getpid,
};
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio::{net::TcpListener, sync::RwLock, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{Level, warn};

#[cfg(not(target_os = "windows"))]
use crate::util::detach_io;
use crate::{
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    data::UserData,
    error::{CliResult, InternalProxyError},
    execution::MIRRORD_EXECUTION_KIND_ENV,
    kube::kube_client_from_layer_config,
    util::create_listen_socket,
};

/// Serializes the resolved [`LayerConfig`] as a JSON object containing only the fields that
/// differ from a freshly generated default config. The session monitor shows the result in
/// its Config tab, so users see only what they (or the environment) customized instead of
/// the full resolved config.
fn config_as_diff(config: &LayerConfig) -> serde_json::Value {
    let actual = match serde_json::to_value(config) {
        Ok(v) => v,
        Err(_) => return serde_json::Value::Null,
    };

    let mut ctx = ConfigContext::default().strict_env(true);
    let Ok(default_cfg) = LayerFileConfig::default().generate_config(&mut ctx) else {
        return actual;
    };
    let default = match serde_json::to_value(&default_cfg) {
        Ok(v) => v,
        Err(_) => return actual,
    };

    json_diff(&actual, &default).unwrap_or(serde_json::Value::Object(Default::default()))
}

/// Recursive JSON diff. Returns `None` when `actual` equals `default`, otherwise returns an
/// object containing only the keys whose values differ, descending into nested objects.
fn json_diff(actual: &serde_json::Value, default: &serde_json::Value) -> Option<serde_json::Value> {
    if actual == default {
        return None;
    }
    match (actual, default) {
        (serde_json::Value::Object(a), serde_json::Value::Object(d)) => {
            let mut out = serde_json::Map::new();
            for (k, v) in a {
                let dv = d.get(k).unwrap_or(&serde_json::Value::Null);
                if let Some(child) = json_diff(v, dv) {
                    out.insert(k.clone(), child);
                }
            }
            if out.is_empty() {
                None
            } else {
                Some(serde_json::Value::Object(out))
            }
        }
        _ => Some(actual.clone()),
    }
}

/// Print the address for the caller (mirrord cli execution flow) so it can pass it
/// back to the layer instances via env var.
fn print_addr(listener: &TcpListener) -> io::Result<()> {
    let addr = listener.local_addr()?;
    println!("{addr}\n");
    Ok(())
}

/// Owns the CI-only signal bridge for the lifetime of the internal proxy setup and run.
pub(crate) struct IntProxyShutdown {
    pub(super) token: CancellationToken,
    signal_task: Option<JoinHandle<()>>,
}

impl Drop for IntProxyShutdown {
    fn drop(&mut self) {
        if let Some(signal_task) = self.signal_task.take() {
            signal_task.abort();
        }
    }
}

#[cfg(unix)]
const CI_SHUTDOWN_WATCHDOG_GRACE: Duration = Duration::from_secs(5);

/// The watchdog stays in intproxy itself, so its self-signal cannot reach a process that has
/// reused a PID after intproxy exited. An OS thread remains runnable if async shutdown stalls.
#[cfg(unix)]
fn arm_ci_shutdown_watchdog(grace: Duration) -> io::Result<()> {
    std::thread::Builder::new()
        .name("mirrord-ci-shutdown-watchdog".to_owned())
        .spawn(move || {
            std::thread::sleep(grace);
            if let Err(error) = kill(getpid(), Signal::SIGKILL) {
                tracing::error!(%error, "Failed to force intproxy shutdown");
            }
        })
        .map(|_| ())
}

/// Installs the signal handler before the CI intproxy pid becomes discoverable by `mirrord ci
/// stop`. The returned owner also removes the receiver task on every natural or setup-error exit.
pub(crate) fn install_ci_shutdown_handler(
    mirrord_for_ci: bool,
) -> Result<IntProxyShutdown, InternalProxyError> {
    let token = CancellationToken::new();
    #[cfg(unix)]
    let signal_task = if mirrord_for_ci {
        let mut sigterm =
            signal(SignalKind::terminate()).map_err(InternalProxyError::SignalHandler)?;
        let shutdown = token.clone();
        Some(tokio::spawn(async move {
            if sigterm.recv().await.is_some() {
                if let Err(error) = arm_ci_shutdown_watchdog(CI_SHUTDOWN_WATCHDOG_GRACE) {
                    tracing::error!(%error, "Failed to arm intproxy shutdown watchdog");
                }
                shutdown.cancel();
            }
        }))
    } else {
        None
    };
    #[cfg(not(unix))]
    let signal_task = {
        let _ = mirrord_for_ci;
        None
    };

    Ok(IntProxyShutdown { token, signal_task })
}

/// Starts the session monitor API server if enabled.
///
/// `@reporter`: a reference to the reporter for chaos metrics which will not prevent the session
/// monitor from being cancelled. To skip reporting chaos metrics (for example in tests) use
/// [`Weak::new()`].
async fn start_session_monitor(
    config: &LayerConfig,
    is_operator: bool,
    reporter: Weak<RwLock<ChaosAnalyticsReporter>>,
    session_id: String,
    required_for_db_portforwards: bool,
) -> (MonitorTx, ChaosWatcherRx) {
    use tokio::sync::watch;

    let (chaos_tx, chaos_rx) = watch::channel(Default::default());

    if !config.api && !required_for_db_portforwards {
        return (MonitorTx::disabled(), ChaosWatcherRx::new(chaos_rx));
    }

    let (tx, _rx) =
        tokio::sync::broadcast::channel::<mirrord_intproxy::session_monitor::MonitorEvent>(256);
    let api_monitor_rx = tx.subscribe();
    let proxy_monitor_tx = MonitorTx::from_sender(tx.clone());
    let api_monitor_tx = MonitorTx::from_sender(tx);

    let target_name = config
        .target
        .path
        .as_ref()
        .map(|t| t.to_string())
        .unwrap_or_else(|| "targetless".to_owned());

    let namespace = match &config.target.namespace {
        Some(namespace) => Some(namespace.clone()),
        None => match kube_client_from_layer_config(config).await {
            Ok(client) => Some(client.default_namespace().to_owned()),
            Err(error) => {
                tracing::debug!(
                    ?error,
                    "Failed to resolve effective namespace from kube client"
                );
                None
            }
        },
    };

    let context = config.kube_context.clone().or_else(|| {
        kube::config::Kubeconfig::read()
            .ok()
            .and_then(|kubeconfig| kubeconfig.current_context)
    });

    let config_value = config_as_diff(config);

    let session_info = SessionInfo {
        session_id: session_id.clone(),
        key: Some(config.key.as_str().to_owned()),
        target: target_name,
        namespace,
        context,
        started_at: humantime::format_rfc3339(std::time::SystemTime::now()).to_string(),
        mirrord_version: env!("CARGO_PKG_VERSION").to_owned(),
        is_operator,
        processes: Vec::new(),
        port_subscriptions: Vec::new(),
        config: config_value,
    };

    let shutdown = CancellationToken::new();

    let sessions_dir = home_dir().map(|home_dir| home_dir.join(".mirrord").join("sessions"));

    tokio::spawn(async move {
        let Some(sessions_dir) = sessions_dir else {
            tracing::warn!(
                "Could not determine home directory; skipping session monitor API server"
            );
            return;
        };
        if let Err(error) = mirrord_intproxy::session_monitor::api::start_api_server(
            sessions_dir,
            session_info,
            api_monitor_tx,
            api_monitor_rx,
            shutdown,
            ChaosWatcherTx::new(chaos_tx),
            reporter,
        )
        .await
        {
            tracing::warn!(%error, "Session monitor API server failed");
        }
    });

    (proxy_monitor_tx, ChaosWatcherRx::new(chaos_rx))
}

/// Main entry point for the internal proxy.
/// It listens for inbound layer connect and forwards to agent.
#[tracing::instrument(level = Level::INFO, skip_all, err)]
pub(crate) async fn proxy(
    config: LayerConfig,
    listen_port: u16,
    watch: drain::Watch,
    user_data: &UserData,
    shutdown_handler: IntProxyShutdown,
) -> Result<(), InternalProxyError> {
    tracing::info!(
        ?config,
        listen_port,
        version = env!("CARGO_PKG_VERSION"),
        "Starting mirrord-intproxy",
    );

    // Held for the whole session, so that the files copied for `feature.fs.prefetch` are removed
    // however this function returns.
    #[cfg(unix)]
    let _prefetched_files = crate::prefetch::PrefetchedFilesGuard::from_env();

    // According to https://wilsonmar.github.io/maximum-limits/ this is the limit on macOS
    // so we assume Linux can be higher and set to that.
    #[cfg(not(target_os = "windows"))]
    if let Err(error) = setrlimit(Resource::RLIMIT_NOFILE, 12288, 12288) {
        warn!(%error, "Failed to set the file descriptor limit");
    }

    let agent_connect_info = env::var_os(AGENT_CONNECT_INFO_ENV_KEY)
        .ok_or(InternalProxyError::MissingConnectInfo)
        .and_then(|var| {
            #[cfg(target_os = "windows")]
            let var = var.to_string_lossy();
            serde_json::from_slice(var.as_bytes()).map_err(|error| {
                InternalProxyError::DeseralizeConnectInfo(
                    String::from_utf8_lossy(var.as_bytes()).into_owned(),
                    error,
                )
            })
        })?;

    let execution_kind = std::env::var(MIRRORD_EXECUTION_KIND_ENV)
        .ok()
        .and_then(|execution_kind| execution_kind.parse().ok())
        .unwrap_or_default();
    let container_mode = crate::util::intproxy_container_mode();

    let mut analytics = if container_mode {
        AnalyticsReporter::only_error(
            config.telemetry,
            execution_kind,
            watch,
            user_data.machine_id(),
            Some(config.key.as_str().to_owned()),
        )
    } else {
        AnalyticsReporter::new(
            config.telemetry,
            execution_kind,
            watch,
            user_data.machine_id(),
            Some(config.key.as_str().to_owned()),
        )
    };
    (&config).collect_analytics(analytics.get_mut());
    if let Some(correlation_id) = read_correlation_id_from_env() {
        analytics.get_mut().add("correlation_id", correlation_id);
    }
    if let Some((major, minor)) = read_kube_version_from_env() {
        analytics.get_mut().add("kube_version_major", major);
        analytics.get_mut().add("kube_version_minor", minor);
    }

    let operator_session_id = if let AgentConnectInfo::Operator(session) = &agent_connect_info {
        Some(session.id())
    } else {
        None
    };

    // The agent is spawned and our parent process already established a connection.
    // However, the parent process (`exec` or `ext` command) is free to exec/exit as soon as it
    // reads the TCP listener address from our stdout. We open our own connection with the agent
    // **before** this happens to ensure that the agent does not prematurely exit.
    // We also perform initial ping pong round to ensure that k8s runtime actually made connection
    // with the agent (it's a must, because port forwarding may be done lazily).
    let is_operator = matches!(&agent_connect_info, AgentConnectInfo::Operator(_));
    let mut agent_conn =
        connect_and_ping(&config, agent_connect_info.clone(), &mut analytics).await?;
    let local_session_id =
        env::var("MIRRORD_SESSION_ID").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
    let needs_db_portforwards = config.feature.db_branches.is_empty().not();

    // Keep the only strong reference in the intproxy so analytics are flushed when the session
    // ends, while the session monitor receives a weak reference for chaos metrics.
    let chaos_reporter = Arc::new(RwLock::new(ChaosAnalyticsReporter::new(analytics)));
    let (monitor_tx, chaos_rx) = start_session_monitor(
        &config,
        is_operator,
        Arc::downgrade(&chaos_reporter),
        local_session_id.clone(),
        needs_db_portforwards,
    )
    .await;
    if needs_db_portforwards
        && let Some(session_id) = operator_session_id
        && let Ok(daemon) = crate::ui::ensure_daemon().await.inspect_err(|error| {
            tracing::warn!(%error, "failed to start the local mirrord daemon");
        })
        && let Err(err) = db_portforwards::setup(
            &config,
            &mut agent_conn,
            session_id,
            &local_session_id,
            config.key.as_str(),
            agent_connect_info,
            &daemon,
        )
        .await
    {
        tracing::warn!(%err, "failed to set up DB branch port forwards, continuing without them");
    }

    // Let it assign address for us then print it for the user.
    let listener = create_listen_socket(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), listen_port))
        .map_err(InternalProxyError::ListenerSetup)?;
    let shutdown = shutdown_handler.token.clone();
    print_addr(&listener).map_err(InternalProxyError::ListenerSetup)?;

    #[cfg(not(target_os = "windows"))]
    if container_mode.not() {
        unsafe { detach_io() }.map_err(InternalProxyError::SetSid)?;
    }

    let first_connection_timeout = Duration::from_secs(config.internal_proxy.start_idle_timeout);
    let consecutive_connection_timeout = Duration::from_secs(config.internal_proxy.idle_timeout);
    let ping_interval = Duration::from_secs(config.internal_proxy.ping_interval.max(1));
    let process_logging_interval =
        Duration::from_secs(config.internal_proxy.process_logging_interval);

    let sip_x64_fallback_count = Arc::new(AtomicU32::new(0));
    let res = IntProxy::new_with_connection(
        agent_conn,
        listener,
        config.feature.fs.readonly_file_buffer,
        config
            .feature
            .network
            .incoming
            .tls_delivery
            .or(config.feature.network.incoming.https_delivery)
            .unwrap_or_default(),
        IntProxyIntervals {
            ping: ping_interval,
            process_logging: process_logging_interval,
        },
        &config.experimental,
        monitor_tx,
        chaos_rx,
    )
    .with_sip_x64_fallback_count(sip_x64_fallback_count.clone())
    .run_with_shutdown(
        first_connection_timeout,
        consecutive_connection_timeout,
        shutdown,
    )
    .await
    .map_err(From::from);

    #[cfg(target_os = "macos")]
    {
        let sip_x64_fallback_count = sip_x64_fallback_count.load(Ordering::Relaxed);
        if sip_x64_fallback_count > 0 {
            chaos_reporter
                .write()
                .await
                .set_sip_x64_fallback_count(sip_x64_fallback_count);
        }
    }

    if res.is_err()
        && tokio::time::timeout(Duration::from_secs(1), async {
            chaos_reporter
                .write()
                .await
                .set_inner_error(AnalyticsError::IntProxyFirstConnection);
        })
        .await
        .is_err()
    {
        warn!("Error could not be set in analytics")
    };

    res
}

/// Creates a connection with the agent and handles one round of ping pong.
#[tracing::instrument(level = Level::TRACE, skip(config, analytics))]
pub(crate) async fn connect_and_ping(
    config: &LayerConfig,
    connect_info: AgentConnectInfo,
    analytics: &mut AnalyticsReporter,
) -> CliResult<AgentConnection, InternalProxyError> {
    let mut agent_conn = AgentConnection::new(config, connect_info, analytics).await?;

    agent_conn.connection.send(ClientMessage::Ping).await;

    loop {
        match agent_conn.connection.recv().await {
            Some(DaemonMessage::Pong) => break Ok(agent_conn),
            Some(DaemonMessage::OperatorPing(id)) => {
                agent_conn
                    .connection
                    .send(ClientMessage::OperatorPong(id))
                    .await;
            }
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
                break Err(InternalProxyError::InitialPingPongFailed(format!(
                    "agent closed connection with message: {reason}"
                )));
            }

            message @ Some(DaemonMessage::UdpOutgoing(_))
            | message @ Some(DaemonMessage::Tcp(_))
            | message @ Some(DaemonMessage::TcpSteal(_))
            | message @ Some(DaemonMessage::TcpOutgoing(_))
            | message @ Some(DaemonMessage::SeqpacketOutgoing(_))
            | message @ Some(DaemonMessage::File(_))
            | message @ Some(DaemonMessage::LogMessage(_))
            | message @ Some(DaemonMessage::GetEnvVarsResponse(_))
            | message @ Some(DaemonMessage::GetAddrInfoResponse(_))
            | message @ Some(DaemonMessage::PauseTarget(_))
            | message @ Some(DaemonMessage::SwitchProtocolVersionResponse(_))
            | message @ Some(DaemonMessage::Vpn(_))
            | message @ Some(DaemonMessage::ReverseDnsLookup(_)) => {
                break Err(InternalProxyError::InitialPingPongFailed(format!(
                    "agent sent an unexpected message: {message:?}"
                )));
            }
            None => {
                break Err(InternalProxyError::InitialPingPongFailed(
                    "agent unexpectedly closed connection".to_owned(),
                ));
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{env, os::unix::process::ExitStatusExt, process::Stdio, time::Duration};

    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    use tokio::{
        io::{AsyncBufReadExt, BufReader},
        process::Command,
    };

    use super::{arm_ci_shutdown_watchdog, install_ci_shutdown_handler};

    /// The test runner must be a separate process because the watchdog's signal kills its owner.
    #[test]
    fn ci_shutdown_watchdog_worker() {
        if env::var_os("MIRRORD_CI_WATCHDOG_TEST_WORKER").is_none() {
            return;
        }

        arm_ci_shutdown_watchdog(Duration::from_millis(100)).unwrap();
        loop {
            std::thread::park();
        }
    }

    #[tokio::test]
    async fn ci_shutdown_signal_worker() {
        if env::var_os("MIRRORD_CI_SHUTDOWN_SIGNAL_TEST_WORKER").is_none() {
            return;
        }

        let shutdown = install_ci_shutdown_handler(true).unwrap();
        println!("ready");
        shutdown.token.cancelled().await;
    }

    /// A normal CI shutdown arms the watchdog before notifying the proxy and exits without
    /// waiting for the watchdog's forced termination.
    #[tokio::test]
    async fn ci_shutdown_signal_cancels_intproxy() {
        let mut child = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "internal_proxy::tests::ci_shutdown_signal_worker",
                "--nocapture",
            ])
            .env("MIRRORD_CI_SHUTDOWN_SIGNAL_TEST_WORKER", "1")
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut line = String::new();
            loop {
                line.clear();
                assert_ne!(stdout.read_line(&mut line).await.unwrap(), 0);
                if line.trim() == "ready" {
                    break;
                }
            }
        })
        .await
        .expect("intproxy installs its signal handler");

        kill(Pid::from_raw(child.id().unwrap() as i32), Signal::SIGTERM).unwrap();
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("intproxy exits after cancellation")
            .unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn ci_shutdown_watchdog_kills_stalled_intproxy() {
        let mut child = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "internal_proxy::tests::ci_shutdown_watchdog_worker",
            ])
            .env("MIRRORD_CI_WATCHDOG_TEST_WORKER", "1")
            .stdout(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();

        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .expect("watchdog stops the stalled process")
            .unwrap();
        assert_eq!(
            status.signal(),
            Some(nix::sys::signal::Signal::SIGKILL as i32)
        );
    }
}
