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

#[cfg(not(target_os = "windows"))]
use std::os::unix::ffi::OsStrExt;
use std::{
    collections::{HashMap, HashSet},
    env,
    fmt::Write as _,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::Not,
    sync::Arc,
    time::Duration,
};

use mirrord_analytics::{AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::{
    LayerConfig,
    feature::database_branches::{
        ConnectionParamsVars, ConnectionSource, DatabaseBranchConfig, DatabaseBranchesConfig,
        ParamSource, TargetEnvironmentVariableSource,
    },
};
use mirrord_intproxy::{
    IntProxy,
    agent_conn::{AgentConnectInfo, AgentConnection},
};
use mirrord_operator::client::database_branches::resolve_branch_id;
use mirrord_progress::NullProgress;
use mirrord_protocol::{
    ClientMessage, DaemonMessage, GetEnvVarsRequest, LogLevel, LogMessage,
    outgoing::tcp::DaemonTcpOutgoing,
};
#[cfg(not(target_os = "windows"))]
use nix::sys::resource::{Resource, setrlimit};
use tokio::net::TcpListener;
use tracing::Level;
#[cfg(not(target_os = "windows"))]
use tracing::warn;
use url::Url;

#[cfg(not(target_os = "windows"))]
use crate::util::detach_io;
use crate::{
    config::RemoteAddr,
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    db_branches::{Portforward, PortforwardSession, portforward_session_dir},
    error::{CliResult, InternalProxyError},
    execution::MIRRORD_EXECUTION_KIND_ENV,
    port_forward,
    user_data::UserData,
    util::create_listen_socket,
};

/// Print the address for the caller (mirrord cli execution flow) so it can pass it
/// back to the layer instances via env var.
fn print_addr(listener: &TcpListener) -> io::Result<()> {
    let addr = listener.local_addr()?;
    println!("{addr}\n");
    Ok(())
}

/// Main entry point for the internal proxy.
/// It listens for inbound layer connect and forwards to agent.
#[tracing::instrument(level = Level::INFO, skip_all, err)]
pub(crate) async fn proxy(
    config: LayerConfig,
    listen_port: u16,
    watch: drain::Watch,
    user_data: &UserData,
) -> Result<(), InternalProxyError> {
    tracing::info!(
        ?config,
        listen_port,
        version = env!("CARGO_PKG_VERSION"),
        "Starting mirrord-intproxy",
    );

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
        )
    } else {
        AnalyticsReporter::new(
            config.telemetry,
            execution_kind,
            watch,
            user_data.machine_id(),
        )
    };
    (&config).collect_analytics(analytics.get_mut());

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
    let mut agent_conn = connect_and_ping(&config, agent_connect_info, &mut analytics).await?;

    if config.feature.db_branches.is_empty().not()
        && let Some(session_id) = operator_session_id
    {
        setup_db_portforwards(
            &config.feature.db_branches,
            &mut agent_conn,
            session_id,
            config.key.as_str(),
        )
        .await
        .map_err(InternalProxyError::DbBranchPortforwardsFailed)?;
    }

    // Let it assign address for us then print it for the user.
    let listener = create_listen_socket(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), listen_port))
        .map_err(InternalProxyError::ListenerSetup)?;
    print_addr(&listener).map_err(InternalProxyError::ListenerSetup)?;

    #[cfg(not(target_os = "windows"))]
    if container_mode.not() {
        unsafe { detach_io() }.map_err(InternalProxyError::SetSid)?;
    }

    let first_connection_timeout = Duration::from_secs(config.internal_proxy.start_idle_timeout);
    let consecutive_connection_timeout = Duration::from_secs(config.internal_proxy.idle_timeout);
    let process_logging_interval =
        Duration::from_secs(config.internal_proxy.process_logging_interval);

    IntProxy::new_with_connection(
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
        process_logging_interval,
        &config.experimental,
    )
    .run(first_connection_timeout, consecutive_connection_timeout)
    .await
    .map_err(From::from)
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
                    "agent unexpectedly closed connection".to_string(),
                ));
            }
        }
    }
}

async fn setup_db_portforwards(
    config: &DatabaseBranchesConfig,
    conn: &mut AgentConnection,
    session_id: u64,
    key: &str,
) -> Result<(), String> {
    let mut portforwards = HashSet::new();

    #[derive(PartialEq, Eq, Hash, Debug, Clone)]
    enum Envs {
        Url(String),
        Params {
            host: String,
            port: String,
            user: Option<String>,
            password: Option<String>,
            database: Option<String>,
            scheme: Option<&'static str>,
        },
    }

    #[derive(PartialEq, Eq, Hash, Debug)]
    struct Pf {
        envs: Envs,
        db_id: String,
    }

    enum ConnInfo {
        /// The original URL with host:port to be replaced with local address.
        ReplaceInUrl(Url),
        /// All params available to build a URL from scratch.
        BuildUrl {
            scheme: &'static str,
            user: String,
            password: String,
            database: Option<String>,
        },
        /// ADO.NET-style connection string for MSSQL.
        BuildMssql {
            user: String,
            password: String,
            database: Option<String>,
        },
        /// Fall back to just the socket address.
        HostPort,
    }

    struct PortMapping {
        db_id: String,
        conn_info: ConnInfo,
    }

    // Extract out the envs we want to fetch
    for branch in config.iter() {
        let (base, scheme) = match branch {
            DatabaseBranchConfig::Mongodb(db) => (&db.base, Some("mongodb")),
            DatabaseBranchConfig::Mysql(db) => (&db.base, Some("mysql")),
            DatabaseBranchConfig::Pg(db) => (&db.base, Some("postgresql")),
            DatabaseBranchConfig::Mssql(db) => (&db.base, Some("mssql")),
            DatabaseBranchConfig::Redis(_) => continue,
        };
        let envs = match &base.connection {
            ConnectionSource::Url { url } => match url {
                TargetEnvironmentVariableSource::Env { variable, .. }
                | TargetEnvironmentVariableSource::EnvFrom { variable, .. } => {
                    Envs::Url(variable.clone())
                }
                TargetEnvironmentVariableSource::Secret { .. } => {
                    continue;
                }
            },
            ConnectionSource::FlatUrl { url, .. } => Envs::Url(url.clone()),
            ConnectionSource::Params(config) => {
                let ConnectionParamsVars {
                    host: Some(host),
                    port: Some(port),
                    user,
                    password,
                    database,
                } = &config.params
                else {
                    continue;
                };

                let (host, port) = match (host, port) {
                    (ParamSource::Variable(host), ParamSource::Variable(port)) => {
                        (host.clone(), port.clone())
                    }
                    // Listing the secret case explicitly so variants
                    // added in the future are not silently discarded.
                    (ParamSource::Secret { .. }, _) | (_, ParamSource::Secret { .. }) => continue,
                };

                let user = user
                    .as_ref()
                    .and_then(ParamSource::as_variable)
                    .map(str::to_owned);
                let password = password
                    .as_ref()
                    .and_then(ParamSource::as_variable)
                    .map(str::to_owned);
                let database = database
                    .as_ref()
                    .and_then(ParamSource::as_variable)
                    .map(str::to_owned);

                Envs::Params {
                    host,
                    port,
                    user,
                    password,
                    database,
                    scheme,
                }
            }
        };
        let db_id = resolve_branch_id(&base.id, key, &NullProgress).into();
        portforwards.insert(Pf { envs, db_id });
    }
    let env_vars_select = portforwards
        .iter()
        .flat_map(|pf| match &pf.envs {
            Envs::Url(u) => vec![u.clone()],
            Envs::Params {
                host,
                port,
                user,
                password,
                database,
                ..
            } => [
                Some(host),
                Some(port),
                user.as_ref(),
                password.as_ref(),
                database.as_ref(),
            ]
            .into_iter()
            .flatten()
            .cloned()
            .collect(),
        })
        .collect();

    // Fetch envs
    conn.connection
        .send(ClientMessage::GetEnvVarsRequest(GetEnvVarsRequest {
            env_vars_filter: Default::default(),
            env_vars_select,
        }))
        .await;

    let vars = match conn.connection.recv().await {
        Some(DaemonMessage::GetEnvVarsResponse(Ok(env_vars))) => env_vars,
        Some(DaemonMessage::GetEnvVarsResponse(Err(err))) => {
            return Err(format!("Error response from agent: {err}"));
        }
        Some(other) => return Err(format!("Unexpected message received from agent: {other:?}")),
        None => return Err("Agent connection dropped unexpectedly".into()),
    };

    // Build db_id, host, port, and connection info mappings from the envs
    let port_mappings: HashMap<_, _> = portforwards.into_iter().filter_map(|pf| -> Option<_> {
        let (host, port, conn_info) = match pf.envs {
            Envs::Url(url_var) => {
                let url = vars.get(&url_var)?
                    .parse::<Url>()
                    .inspect_err(|e| tracing::warn!(?e, env_var = %url_var, "failed to parse url for db branch connection string, portforward will not be made"))
                    .ok()?;

                let host = url.host_str()?;

                let host = host
                    .parse()
                    .map(RemoteAddr::Ip)
                    .unwrap_or_else(|_| RemoteAddr::Hostname(host.to_string()));

                let port = url.port()?;

                (host, port, ConnInfo::ReplaceInUrl(url))
            },
            Envs::Params { host: host_var, port: port_var, user, password, database, scheme } => {
                let port_val: u16 = vars
                    .get(&port_var)?
                    .parse()
                    .inspect_err(|e| tracing::warn!(env_var = %port_var, ?e, "failed to parse u16 from db branch port env var, portforward will not be made"))
                    .ok()?;

                let host_val = vars.get(&host_var)?;
                let remote_host = host_val
                    .parse()
                    .map(RemoteAddr::Ip)
                    .unwrap_or_else(|_| RemoteAddr::Hostname(host_val.to_owned()));

                let conn_info = scheme
                    .zip(user)
                    .zip(password)
                    .and_then(|((scheme, user_var), pass_var)| {
                        let user = vars.get(&user_var)?.clone();
                        let password = vars.get(&pass_var)?.clone();
                        let database = database.and_then(|d| vars.get(&d)).cloned();
                        Some(if scheme == "mssql" {
                            ConnInfo::BuildMssql {
                                user,
                                password,
                                database,
                            }
                        } else {
                            ConnInfo::BuildUrl {
                                scheme,
                                user,
                                password,
                                database,
                            }
                        })
                    })
                    .unwrap_or(ConnInfo::HostPort);

                (remote_host, port_val, conn_info)
            }
        };
        Some(((host, port), PortMapping { db_id: pf.db_id, conn_info }))
    }).collect();

    let connections_state = Arc::new(port_forward::ConnectionsState::default());
    let connections_state_2 = Arc::clone(&connections_state);

    // Split agent connection, redirect relevant messages to portforwarder
    let pf_rx = conn.connection.split_incoming(64, move |inc| {
        let DaemonMessage::TcpOutgoing(tcp) = inc else {
            return false;
        };
        match tcp {
            DaemonTcpOutgoing::Connect(_) => false, // Portforwarder doesn't use V1
            DaemonTcpOutgoing::Read(read) => match read {
                Ok(read) => connections_state
                    .ongoing
                    .lock()
                    .unwrap()
                    .contains(&read.connection_id),
                Err(err) => {
                    // As of writing this, the agent never sends any
                    // `DaemonTcpOutgoing::Read(Err)` messages. I
                    // don't even think it would make any sense, since
                    // it would contain no associated connection id.
                    // Both intproxy and port_forwarder handle it by
                    // just erroring out and quitting, so passing it
                    // to intproxy unconditionally should be
                    // acceptable.
                    tracing::error!(?err, "Received DaemonTcpOutgoing::Read with Err");
                    false
                }
            },
            DaemonTcpOutgoing::Close(id) => connections_state.ongoing.lock().unwrap().contains(id),
            DaemonTcpOutgoing::ConnectV2(cv2) => {
                connections_state.pending.lock().unwrap().contains(&cv2.uid)
            }
        }
    });

    let localhost_ephemeral_port = SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0));

    let mut portforwarder = port_forward::PortForwarder::new(
        conn.connection.tx_handle(),
        pf_rx,
        port_mappings
            .keys()
            .map(|rmt| (localhost_ephemeral_port, rmt.clone())),
        Some(connections_state_2),
    )
    .await
    .map_err(|e| format!("Error setting up PortForwarder: {e}"))?;

    let portforward_mappings: Vec<_> = portforwarder
        .listeners()
        .filter_map(|(local, remote)| {
            let mapping = port_mappings.get(remote)?;
            let connection_string = match &mapping.conn_info {
                ConnInfo::ReplaceInUrl(url) => {
                    let mut url = url.clone();
                    let host = match local.ip() {
                        IpAddr::V4(v4) => v4.to_string(),
                        IpAddr::V6(v6) => format!("[{v6}]"),
                    };
                    if url.set_host(Some(&host)).is_ok() && url.set_port(Some(local.port())).is_ok()
                    {
                        url.to_string()
                    } else {
                        local.to_string()
                    }
                }
                ConnInfo::BuildUrl {
                    scheme,
                    user,
                    password,
                    database,
                } => {
                    let mut url = Url::parse(&format!("{scheme}://localhost")).unwrap();
                    let host = match local.ip() {
                        IpAddr::V4(v4) => v4.to_string(),
                        IpAddr::V6(v6) => format!("[{v6}]"),
                    };
                    url.set_host(Some(&host)).unwrap();
                    url.set_port(Some(local.port())).unwrap();
                    url.set_username(user).unwrap();
                    url.set_password(Some(password)).unwrap();
                    if let Some(db) = database {
                        url.set_path(&format!("/{db}"));
                    }
                    if *scheme == "mongodb" {
                        url.query_pairs_mut().append_pair("authSource", "admin");
                    }
                    url.to_string()
                }
                ConnInfo::BuildMssql {
                    user,
                    password,
                    database,
                } => {
                    let host = match local.ip() {
                        IpAddr::V4(v4) => v4.to_string(),
                        IpAddr::V6(v6) => format!("[{v6}]"),
                    };
                    let mut conn = format!(
                        "Server={host},{};User Id={user};Password={password}",
                        local.port()
                    );
                    if let Some(db) = database {
                        write!(conn, ";Database={db}").unwrap();
                    }
                    conn.push(';');
                    conn
                }
                ConnInfo::HostPort => local.to_string(),
            };
            Some(Portforward {
                db_id: mapping.db_id.clone(),
                connection_string,
            })
        })
        .collect();

    struct PortforwardFileGuard {
        path: std::path::PathBuf,
    }

    impl Drop for PortforwardFileGuard {
        fn drop(&mut self) {
            if let Err(err) = std::fs::remove_file(&self.path) {
                tracing::warn!(?err, path = %self.path.display(), "failed to remove portforward session file");
            }
        }
    }

    let pf_guard = {
        let session = PortforwardSession {
            portforwards: portforward_mappings,
            key: key.to_owned(),
            session_id,
        };

        let pf_dir = portforward_session_dir();
        tokio::fs::create_dir_all(&pf_dir)
            .await
            .map_err(|e| format!("Failed to create portforward directory: {e}"))?;

        let pf_path = pf_dir.join(format!("{}.json", std::process::id()));
        let json = serde_json::to_vec(&session)
            .map_err(|e| format!("Failed to serialize portforward session: {e}"))?;
        tokio::fs::write(&pf_path, json)
            .await
            .map_err(|e| format!("Failed to write portforward session file: {e}"))?;

        PortforwardFileGuard { path: pf_path }
    };

    tokio::spawn(async move {
        let _pf_guard = pf_guard;
        if let Err(err) = portforwarder.run().await {
            tracing::error!(?err, "DB branch portforwarding failed");
        }
    });

    Ok(())
}
