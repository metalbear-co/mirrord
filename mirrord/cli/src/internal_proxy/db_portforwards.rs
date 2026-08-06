use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    fs::OpenOptions,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use fs4::fs_std::FileExt;
use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use mirrord_analytics::AnalyticsReporter;
use mirrord_config::LayerConfig;
use mirrord_config::feature::database_branches::{
    ConnectionParamsVars, ConnectionSource, DatabaseBranchConfig, DatabaseBranchesConfig,
    ParamSource, RedisBranchConfig, TargetEnvironmentVariableSource,
};
use mirrord_intproxy::agent_conn::AgentConnection;
use mirrord_operator::client::database_branches::resolve_branch_id;
use mirrord_progress::NullProgress;
use mirrord_protocol::{ClientMessage, DaemonMessage, GetEnvVarsRequest, ResponseError};
use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::{
    config::RemoteAddr,
    connection::AGENT_CONNECT_INFO_ENV_KEY,
    db_branches::{Portforward, PortforwardSession, portforward_session_dir},
    error::InternalProxyError,
    port_forward,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::Command,
};

#[derive(Debug, Error)]
pub(crate) enum SetupError {
    #[error("error response from agent: {0}")]
    AgentError(#[from] ResponseError),

    #[error("unexpected message received from agent: {0:?}")]
    UnexpectedAgentMessage(Box<DaemonMessage>),

    #[error("agent connection dropped unexpectedly")]
    AgentConnectionDropped,

    #[error("failed to set up port forwarder: {0}")]
    PortForwarder(#[from] port_forward::PortForwardError),

    #[error("failed to create portforward directory: {0}")]
    CreateDir(std::io::Error),

    #[error("failed to serialize portforward session: {0}")]
    Serialize(#[from] serde_json::Error),

    #[error("failed to write portforward session file: {0}")]
    WriteFile(std::io::Error),
}

#[derive(PartialEq, Eq, Hash, Debug, Clone)]
enum Envs {
    Url(String),
    Params {
        host: String,
        /// Name of the env var holding the port. `None` when host and port share one variable
        /// as `host:port` (Spanner's `SPANNER_EMULATOR_HOST`); then `host`'s value is split.
        port: Option<String>,
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

impl ConnInfo {
    fn connection_string(&self, local: SocketAddr) -> String {
        match self {
            ConnInfo::ReplaceInUrl(url) => {
                let mut url = url.clone();
                let host = match local.ip() {
                    IpAddr::V4(v4) => v4.to_string(),
                    IpAddr::V6(v6) => format!("[{v6}]"),
                };
                if url.set_host(Some(&host)).is_ok() && url.set_port(Some(local.port())).is_ok() {
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
        }
    }
}

fn extract_portforward_configs(config: &DatabaseBranchesConfig, key: &str) -> HashSet<Pf> {
    let mut portforwards = HashSet::new();

    for branch in config.iter() {
        // Spanner redirects through a single `host:port` env var (`SPANNER_EMULATOR_HOST`) rather
        // than the shared `base.connection` source. It reuses the params path with no separate
        // port var: `port: None` tells the resolver to split the host var's value, and the absent
        // scheme makes it write the branch address back as a bare `host:port`.
        if let DatabaseBranchConfig::Spanner(db) = branch {
            let db_id = resolve_branch_id(&db.base.id, key, &NullProgress).into();
            portforwards.insert(Pf {
                envs: Envs::Params {
                    host: db.emulator_host.clone(),
                    port: None,
                    user: None,
                    password: None,
                    database: None,
                    scheme: None,
                },
                db_id,
            });
            continue;
        }

        let (base, scheme) = match branch {
            DatabaseBranchConfig::Clickhouse(db) => (&db.base, Some("clickhouse")),
            // CockroachDB is PostgreSQL-wire-compatible and the app keeps its PostgreSQL driver,
            // which rejects a `cockroachdb://` scheme, so the branch URL uses `postgresql`.
            DatabaseBranchConfig::Cockroachdb(db) => (&db.base, Some("postgresql")),
            DatabaseBranchConfig::Dynamodb(db) => (&db.base, Some("dynamodb")),
            DatabaseBranchConfig::Mongodb(db) => (&db.base, Some("mongodb")),
            DatabaseBranchConfig::Mysql(db) => (&db.base, Some("mysql")),
            DatabaseBranchConfig::Mariadb(db) => (&db.base, Some("mariadb")),
            DatabaseBranchConfig::Pg(db) => (&db.base, Some("postgresql")),
            DatabaseBranchConfig::Mssql(db) => (&db.base, Some("mssql")),
            DatabaseBranchConfig::Redis(db) => match &**db {
                RedisBranchConfig::Local(_) => continue,
                RedisBranchConfig::Remote(db) => (&db.base, Some("redis")),
            },
            // mirrord knows nothing about a generic branch's protocol, so the portforward
            // address is rendered as a bare `host:port` (no scheme), like Spanner's.
            DatabaseBranchConfig::Generic(db) => (&db.base, None),
            DatabaseBranchConfig::Spanner(_) => unreachable!("handled above"),
        };
        let envs = match &base.connection {
            ConnectionSource::Url { url } => match url {
                TargetEnvironmentVariableSource::Env { variable, .. }
                | TargetEnvironmentVariableSource::EnvFrom { variable, .. } => {
                    Envs::Url(variable.clone())
                }
                TargetEnvironmentVariableSource::Secret { .. }
                | TargetEnvironmentVariableSource::GcpSecretManager { .. } => {
                    continue;
                }
            },
            ConnectionSource::FlatUrl { url, .. } => {
                let Some(first_url) = url.first() else {
                    continue;
                };
                Envs::Url(first_url.clone())
            }
            ConnectionSource::Params(config) => {
                let ConnectionParamsVars {
                    host: Some(host),
                    port: Some(port),
                    user,
                    password,
                    database,
                    extra: _,
                } = &config.params
                else {
                    continue;
                };

                let (Some(host), Some(port)) = (
                    host.first().and_then(ParamSource::as_variable),
                    port.first().and_then(ParamSource::as_variable),
                ) else {
                    continue;
                };
                let (host, port) = (host.to_owned(), port.to_owned());

                let user = user
                    .as_ref()
                    .and_then(|om| om.first())
                    .and_then(ParamSource::as_variable)
                    .map(str::to_owned);
                let password = password
                    .as_ref()
                    .and_then(|om| om.first())
                    .and_then(ParamSource::as_variable)
                    .map(str::to_owned);
                let database = database
                    .as_ref()
                    .and_then(|om| om.first())
                    .and_then(ParamSource::as_variable)
                    .map(str::to_owned);

                Envs::Params {
                    host,
                    port: Some(port),
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

    portforwards
}

fn resolve_port_mappings(
    portforwards: HashSet<Pf>,
    vars: &HashMap<String, String>,
) -> HashMap<(RemoteAddr, u16), PortMapping> {
    portforwards
        .into_iter()
        .filter_map(|pf| -> Option<_> {
            let (host, port, conn_info) = match pf.envs {
                Envs::Url(url_var) => {
                    let url = vars
                        .get(&url_var)?
                        .parse::<Url>()
                        .inspect_err(|e| {
                            tracing::warn!(
                                ?e,
                                env_var = %url_var,
                                "failed to parse url for db branch connection string, \
                                 portforward will not be made"
                            )
                        })
                        .ok()?;

                    let host = url.host_str()?;

                    let host = host
                        .parse()
                        .map(RemoteAddr::Ip)
                        .unwrap_or_else(|_| RemoteAddr::Hostname(host.to_owned()));

                    let port = url.port()?;

                    (host, port, ConnInfo::ReplaceInUrl(url))
                }
                Envs::Params {
                    host: host_var,
                    port: port_var,
                    user,
                    password,
                    database,
                    scheme,
                } => {
                    let (remote_host, port_val) = match port_var {
                        Some(port_var) => {
                            let port_val: u16 = vars
                                .get(&port_var)?
                                .parse()
                                .inspect_err(|e| {
                                    tracing::warn!(
                                        env_var = %port_var,
                                        ?e,
                                        "failed to parse u16 from db branch port env var, \
                                         portforward will not be made"
                                    )
                                })
                                .ok()?;
                            let host_val = vars.get(&host_var)?;
                            let remote_host = host_val
                                .parse()
                                .map(RemoteAddr::Ip)
                                .unwrap_or_else(|_| RemoteAddr::Hostname(host_val.to_owned()));
                            (remote_host, port_val)
                        }
                        None => {
                            // Host and port share one var as `host:port`; split on the last colon
                            // so IPv6 hosts (which contain colons) still parse.
                            let value = vars.get(&host_var)?;
                            let (host_str, port_str) = value.rsplit_once(':')?;
                            let port_val: u16 = port_str
                                .parse()
                                .inspect_err(|e| {
                                    tracing::warn!(
                                        env_var = %host_var,
                                        ?e,
                                        "failed to parse port from db branch host:port env var, \
                                         portforward will not be made"
                                    )
                                })
                                .ok()?;
                            let remote_host = host_str
                                .parse()
                                .map(RemoteAddr::Ip)
                                .unwrap_or_else(|_| RemoteAddr::Hostname(host_str.to_owned()));
                            (remote_host, port_val)
                        }
                    };

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
            Some((
                (host, port),
                PortMapping {
                    db_id: pf.db_id,
                    conn_info,
                },
            ))
        })
        .collect()
}

pub(super) async fn setup(
    config: &LayerConfig,
    conn: &mut AgentConnection,
    session_id: u64,
    key: &str,
) -> Result<(), SetupError> {
    let portforwards = extract_portforward_configs(&config.feature.db_branches, key);

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
                port.as_ref(),
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

    conn.connection
        .send(ClientMessage::GetEnvVarsRequest(GetEnvVarsRequest {
            env_vars_filter: Default::default(),
            env_vars_select,
        }))
        .await;

    let vars = match conn.connection.recv().await {
        Some(DaemonMessage::GetEnvVarsResponse(Ok(env_vars))) => env_vars,
        Some(DaemonMessage::GetEnvVarsResponse(Err(err))) => {
            return Err(SetupError::AgentError(err));
        }
        Some(other) => return Err(SetupError::UnexpectedAgentMessage(Box::new(other))),
        None => return Err(SetupError::AgentConnectionDropped),
    };

    let port_mappings = resolve_port_mappings(portforwards, &vars);
    let mut attachments = Vec::with_capacity(port_mappings.len());
    let mut portforward_mappings = Vec::with_capacity(port_mappings.len());

    for ((remote, port), mapping) in port_mappings {
        let remote_host = remote_host(&remote);
        let state_path = manager_state_path(config, &mapping.db_id, &remote_host, port);
        let (local, attachment) = attach_to_manager(&state_path, &remote_host, port).await?;

        portforward_mappings.push(Portforward {
            db_id: mapping.db_id,
            connection_string: mapping.conn_info.connection_string(local),
        });
        attachments.push(attachment);
    }

    struct PortforwardFileGuard {
        path: std::path::PathBuf,
    }

    impl Drop for PortforwardFileGuard {
        fn drop(&mut self) {
            if let Err(err) = std::fs::remove_file(&self.path) {
                tracing::warn!(
                    ?err,
                    path = %self.path.display(),
                    "failed to remove portforward session file"
                );
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
            .map_err(SetupError::CreateDir)?;

        let pf_path = pf_dir.join(format!("{}.json", std::process::id()));
        let json = serde_json::to_vec(&session)?;
        tokio::fs::write(&pf_path, json)
            .await
            .map_err(SetupError::WriteFile)?;

        PortforwardFileGuard { path: pf_path }
    };

    tokio::spawn(async move {
        // Each open control connection is a claim on its manager. Dropping it on intproxy exit
        // removes the claim even when the parent CLI cannot run normal cleanup.
        let _attachments = attachments;
        let _pf_guard = pf_guard;
        std::future::pending::<()>().await;
    });

    Ok(())
}

/// Returns the state-file name for a forward that is safe to share only inside one cluster context.
fn manager_state_path(config: &LayerConfig, db_id: &str, remote_host: &str, remote_port: u16) -> PathBuf {
    let identity = format!(
        "{}|{}|{}|{}|{}",
        config.kube_context.as_deref().unwrap_or_default(),
        config.target.namespace.as_deref().unwrap_or_default(),
        db_id,
        remote_host,
        remote_port,
    );
    let hash = format!("{:x}", Sha256::digest(identity.as_bytes()));
    portforward_session_dir().join(format!("{}.manager.json", &hash[..24]))
}

fn remote_host(remote: &RemoteAddr) -> String {
    match remote {
        RemoteAddr::Ip(ip) => ip.to_string(),
        RemoteAddr::Hostname(host) => host.clone(),
    }
}

/// Keeps a control connection open for the intproxy lifetime. Its closure is the manager's
/// authoritative signal that this session no longer uses the forward.
struct ForwardAttachment {
    _stream: TcpStream,
}

#[derive(Serialize, Deserialize)]
struct ManagerState {
    control_addr: SocketAddr,
    token: String,
}

async fn attach_to_manager(
    state_path: &Path,
    remote_host: &str,
    remote_port: u16,
) -> Result<(SocketAddr, ForwardAttachment), SetupError> {
    if let Some(state) = read_manager_state(state_path).await {
        if let Ok(stream) = TcpStream::connect(state.control_addr).await {
            return attach_to_control(stream, &state.token).await;
        }
    }

    let state_path = state_path.to_owned();
    let remote_host = remote_host.to_owned();
    let lock_path = state_path.with_extension("lock");
    tokio::fs::create_dir_all(
        state_path
            .parent()
            .expect("database branch portforward state file has a parent"),
    )
    .await
    .map_err(SetupError::CreateDir)?;

    // Serialise start-up so concurrent sessions elect one manager instead of binding duplicate
    // local ports for the same branch endpoint.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(lock_path)
        .map_err(SetupError::CreateDir)?;
    loop {
        if lock.try_lock_exclusive().is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let result = async {
        if let Some(state) = read_manager_state(&state_path).await {
            if let Ok(stream) = TcpStream::connect(state.control_addr).await {
                return attach_to_control(stream, &state.token).await;
            }
        }

        // A state file whose manager cannot be reached is stale. The startup lock makes replacing
        // it safe when multiple sessions start at once.
        let _ = tokio::fs::remove_file(&state_path).await;
        spawn_manager(&state_path, &remote_host, remote_port).await?;

        for _ in 0..80 {
            if let Some(state) = read_manager_state(&state_path).await {
                match TcpStream::connect(state.control_addr).await {
                    Ok(stream) => return attach_to_control(stream, &state.token).await,
                    Err(_) => {}
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Err(SetupError::CreateDir(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "database branch portforward manager did not start",
        )))
    }
    .await;

    let _ = FileExt::unlock(&lock);
    result
}

async fn read_manager_state(state_path: &Path) -> Option<ManagerState> {
    let contents = tokio::fs::read(state_path).await.ok()?;
    serde_json::from_slice(&contents).ok()
}

async fn spawn_manager(state_path: &Path, remote_host: &str, remote_port: u16) -> Result<(), SetupError> {
    let exe = std::env::current_exe().map_err(SetupError::CreateDir)?;
    let mut command = Command::new(exe);
    command
        .arg("db-branch-portforwarder")
        .arg("--state")
        .arg(state_path)
        .arg("--remote-host")
        .arg(remote_host)
        .arg("--remote-port")
        .arg(remote_port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    detach_manager_process(&mut command).map_err(SetupError::CreateDir)?;

    let mut child = command.spawn().map_err(SetupError::CreateDir)?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

/// The protocol is intentionally just TCP and a small JSON state file. This is shared by every
/// supported OS; process detachment is the only platform-specific concern.
async fn attach_to_control(mut stream: TcpStream, token: &str) -> Result<(SocketAddr, ForwardAttachment), SetupError> {
    stream.write_all(token.as_bytes()).await.map_err(SetupError::CreateDir)?;
    stream.write_all(b"\n").await.map_err(SetupError::CreateDir)?;
    let mut response = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        if stream.read(&mut byte).await.map_err(SetupError::CreateDir)? == 0 {
            return Err(SetupError::CreateDir(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "database branch portforward manager closed before responding",
            )));
        }
        if byte[0] == b'\n' {
            break;
        }
        response.push(byte[0]);
    }
    let local = std::str::from_utf8(&response)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| SetupError::CreateDir(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid database branch portforward manager response",
        )))?;
    Ok((local, ForwardAttachment { _stream: stream }))
}

#[cfg(unix)]
fn detach_manager_process(command: &mut Command) -> Result<(), std::io::Error> {
    unsafe {
        command.pre_exec(|| {
            crate::util::reparent_to_init()?;
            crate::util::detach_io()?;
            Ok(())
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn detach_manager_process(_command: &mut Command) -> Result<(), std::io::Error> {
    // A Windows child is independent of its parent. Standard streams are already detached above.
    Ok(())
}

/// Runs as a detached CLI child. It owns both the local TCP listener and a dedicated connection
/// to the agent, so no individual intproxy owns the forward's lifetime.
pub(crate) async fn run_manager(
    state_path: PathBuf,
    remote_host: String,
    remote_port: u16,
) -> Result<(), InternalProxyError> {
    let config = mirrord_config::util::read_resolved_config()?;
    let connect_info = std::env::var(AGENT_CONNECT_INFO_ENV_KEY)
        .map_err(|_| InternalProxyError::MissingConnectInfo)
        .and_then(|value| serde_json::from_str(&value).map_err(|error| InternalProxyError::DeseralizeConnectInfo(value, error)))?;

    let (_signal, watch) = drain::channel();
    let mut analytics = AnalyticsReporter::only_error(
        false,
        Default::default(),
        watch,
        uuid::Uuid::nil(),
        Some(config.key.as_str().to_owned()),
    );
    let mut agent = super::connect_and_ping(&config, connect_info, &mut analytics).await?;
    let remote = remote_host
        .parse::<Ipv4Addr>()
        .map(RemoteAddr::Ip)
        .unwrap_or_else(|_| RemoteAddr::Hostname(remote_host));
    let agent_tx = agent.connection.tx_handle();
    let incoming = agent.connection.split_incoming(64, |_| true);
    let mut forwarder = port_forward::PortForwarder::new(
        agent_tx,
        incoming,
        [(SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)), (remote, remote_port))],
        None,
    )
    .await?;
    let local = forwarder
        .listeners()
        .next()
        .map(|(local, _)| local)
        .expect("a DB branch manager always creates one listener");

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(InternalProxyError::DbBranchPortForwardControl)?;
    let state = ManagerState {
        control_addr: listener.local_addr().map_err(InternalProxyError::DbBranchPortForwardControl)?,
        token: uuid::Uuid::new_v4().to_string(),
    };
    tokio::fs::write(&state_path, serde_json::to_vec(&state).expect("manager state serializes"))
        .await
        .map_err(InternalProxyError::DbBranchPortForwardControl)?;
    let mut forward_task = tokio::spawn(async move { forwarder.run().await });
    let mut clients = FuturesUnordered::new();
    let mut has_client = false;
    let initial_client_timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(initial_client_timeout);

    loop {
        tokio::select! {
            _ = &mut initial_client_timeout, if !has_client => {
                break;
            }
            accepted = listener.accept() => {
                let (mut stream, _) = accepted.map_err(InternalProxyError::DbBranchPortForwardControl)?;
                let token = read_control_line(&mut stream).await?;
                if token != state.token {
                    continue;
                }
                stream
                    .write_all(format!("{local}\n").as_bytes())
                    .await
                    .map_err(InternalProxyError::DbBranchPortForwardControl)?;
                has_client = true;
                clients.push(async move {
                    let mut buffer = [0_u8; 1];
                    while stream.read(&mut buffer).await? != 0 {}
                    Ok::<(), std::io::Error>(())
                }.boxed());
            }
            Some(result) = clients.next(), if !clients.is_empty() => {
                if let Err(error) = result {
                    tracing::debug!(?error, "DB branch portforward control client closed with an error");
                }
                if has_client && clients.is_empty() {
                    break;
                }
            }
            result = &mut forward_task => {
                let _ = tokio::fs::remove_file(&state_path).await;
                return Ok(result
                    .map_err(|error| {
                        InternalProxyError::DbBranchPortForwardControl(std::io::Error::other(error))
                    })??);
            }
        }
    }

    forward_task.abort();
    let _ = forward_task.await;
    let _ = tokio::fs::remove_file(state_path).await;
    Ok(())
}

async fn read_control_line(stream: &mut TcpStream) -> Result<String, InternalProxyError> {
    let mut line = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        if stream.read(&mut byte).await.map_err(InternalProxyError::DbBranchPortForwardControl)? == 0 {
            return Ok(String::new());
        }
        if byte[0] == b'\n' {
            return Ok(String::from_utf8_lossy(&line).into_owned());
        }
        if line.len() >= 128 {
            return Ok(String::new());
        }
        line.push(byte[0]);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mirrord_config::feature::database_branches::{
        CockroachdbBranchConfig, ConnectionParamsConfig, ConnectionParamsVars, ConnectionSource,
        DatabaseBranchBaseConfig, DatabaseBranchConfig, DatabaseBranchesConfig, MysqlBranchConfig,
        ParamSource, TargetEnvironmentVariableSource,
    };

    use super::*;
    use crate::config::RemoteAddr;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    #[tokio::test]
    async fn control_attachment_is_released_when_the_session_disconnects() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut token = Vec::new();
            loop {
                let mut byte = [0_u8; 1];
                stream.read_exact(&mut byte).await.unwrap();
                if byte[0] == b'\n' {
                    break;
                }
                token.push(byte[0]);
            }
            assert_eq!(token, b"test-token");
            stream.write_all(b"127.0.0.1:5432\n").await.unwrap();

            let mut byte = [0_u8; 1];
            assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
        });

        let stream = TcpStream::connect(address).await.unwrap();
        let (_, attachment) = attach_to_control(stream, "test-token").await.unwrap();
        drop(attachment);
        server.await.unwrap();
    }

    fn base(id: Option<&str>, connection: ConnectionSource) -> DatabaseBranchBaseConfig {
        DatabaseBranchBaseConfig {
            id: id.map(str::to_owned),
            name: None,
            ttl_secs: Some(300),
            ttl_mins: None,
            creation_timeout_secs: 60,
            version: None,
            image: None,
            profile: None,
            connection,
        }
    }

    fn mysql(id: Option<&str>, conn: ConnectionSource) -> DatabaseBranchConfig {
        DatabaseBranchConfig::Mysql(Box::new(MysqlBranchConfig {
            base: base(id, conn),
            copy: Default::default(),
            iam_auth: None,
            migrations: None,
        }))
    }

    fn cockroachdb(id: Option<&str>, conn: ConnectionSource) -> DatabaseBranchConfig {
        DatabaseBranchConfig::Cockroachdb(Box::new(CockroachdbBranchConfig {
            base: base(id, conn),
            copy: Default::default(),
            migrations: None,
        }))
    }

    fn url_env(var: &str) -> ConnectionSource {
        ConnectionSource::Url {
            url: TargetEnvironmentVariableSource::Env {
                container: None,
                variable: var.to_owned(),
                value: None,
            },
        }
    }

    // --- extract_portforward_configs ---

    #[test]
    fn extract_url_env() {
        let config = DatabaseBranchesConfig(vec![mysql(Some("db1"), url_env("DB_URL"))]);
        let result = extract_portforward_configs(&config, "key");

        assert_eq!(result.len(), 1);
        let pf = result.into_iter().next().unwrap();
        assert_eq!(pf.envs, Envs::Url("DB_URL".to_owned()));
        assert_eq!(pf.db_id, "db1");
    }

    #[test]
    fn extract_url_secret_skipped() {
        let conn = ConnectionSource::Url {
            url: TargetEnvironmentVariableSource::Secret {
                name: "db-secret".to_owned(),
                key: "url".to_owned(),
                env_var_name: None,
            },
        };
        let config = DatabaseBranchesConfig(vec![mysql(Some("db3"), conn)]);
        assert!(extract_portforward_configs(&config, "key").is_empty());
    }

    #[test]
    fn extract_params_all_variables() {
        let conn = ConnectionSource::Params(Box::new(ConnectionParamsConfig {
            source_type: None,
            params: ConnectionParamsVars {
                host: Some(ParamSource::Variable("H".to_owned()).into()),
                port: Some(ParamSource::Variable("P".to_owned()).into()),
                user: Some(ParamSource::Variable("U".to_owned()).into()),
                password: Some(ParamSource::Variable("PW".to_owned()).into()),
                database: Some(ParamSource::Variable("DB".to_owned()).into()),
                extra: Default::default(),
            },
        }));
        let config = DatabaseBranchesConfig(vec![mysql(Some("db5"), conn)]);
        let result = extract_portforward_configs(&config, "key");

        assert_eq!(result.len(), 1);
        let pf = result.into_iter().next().unwrap();
        assert_eq!(
            pf.envs,
            Envs::Params {
                host: "H".to_owned(),
                port: Some("P".to_owned()),
                user: Some("U".to_owned()),
                password: Some("PW".to_owned()),
                database: Some("DB".to_owned()),
                scheme: Some("mysql"),
            }
        );
    }

    /// CockroachDB is PostgreSQL-wire-compatible, so its params-based branch renders a
    /// `postgresql` scheme rather than a `cockroachdb` one the app's PG driver would reject.
    #[test]
    fn extract_cockroachdb_params_uses_postgresql_scheme() {
        let conn = ConnectionSource::Params(Box::new(ConnectionParamsConfig {
            source_type: None,
            params: ConnectionParamsVars {
                host: Some(ParamSource::Variable("H".to_owned()).into()),
                port: Some(ParamSource::Variable("P".to_owned()).into()),
                user: Some(ParamSource::Variable("U".to_owned()).into()),
                password: Some(ParamSource::Variable("PW".to_owned()).into()),
                database: Some(ParamSource::Variable("DB".to_owned()).into()),
                extra: Default::default(),
            },
        }));
        let config = DatabaseBranchesConfig(vec![cockroachdb(Some("crdb1"), conn)]);
        let result = extract_portforward_configs(&config, "key");

        assert_eq!(result.len(), 1);
        let pf = result.into_iter().next().unwrap();
        assert_eq!(
            pf.envs,
            Envs::Params {
                host: "H".to_owned(),
                port: Some("P".to_owned()),
                user: Some("U".to_owned()),
                password: Some("PW".to_owned()),
                database: Some("DB".to_owned()),
                scheme: Some("postgresql"),
            }
        );
    }

    // --- resolve_port_mappings ---

    #[test]
    fn resolve_url_happy_path() {
        let pf = Pf {
            envs: Envs::Url("DB_URL".to_owned()),
            db_id: "branch-1".to_owned(),
        };
        let vars = HashMap::from([(
            "DB_URL".to_owned(),
            "postgresql://user:pass@db.example.com:5432/mydb".to_owned(),
        )]);

        let result = resolve_port_mappings([pf].into(), &vars);

        assert_eq!(result.len(), 1);
        let key = (RemoteAddr::Hostname("db.example.com".to_owned()), 5432);
        let mapping = result.get(&key).unwrap();
        assert_eq!(mapping.db_id, "branch-1");
        assert!(matches!(mapping.conn_info, ConnInfo::ReplaceInUrl(_)));
    }

    #[test]
    fn resolve_params_build_url() {
        let pf = Pf {
            envs: Envs::Params {
                host: "H".to_owned(),
                port: Some("P".to_owned()),
                user: Some("U".to_owned()),
                password: Some("PW".to_owned()),
                database: Some("DB".to_owned()),
                scheme: Some("postgresql"),
            },
            db_id: "branch-2".to_owned(),
        };
        let vars = HashMap::from([
            ("H".to_owned(), "db.host.com".to_owned()),
            ("P".to_owned(), "5432".to_owned()),
            ("U".to_owned(), "admin".to_owned()),
            ("PW".to_owned(), "secret".to_owned()),
            ("DB".to_owned(), "mydb".to_owned()),
        ]);

        let result = resolve_port_mappings([pf].into(), &vars);

        assert_eq!(result.len(), 1);
        let key = (RemoteAddr::Hostname("db.host.com".to_owned()), 5432);
        let mapping = result.get(&key).unwrap();
        assert_eq!(mapping.db_id, "branch-2");
        assert!(matches!(
            mapping.conn_info,
            ConnInfo::BuildUrl {
                scheme: "postgresql",
                ..
            }
        ));
    }

    #[test]
    fn resolve_params_mssql_build() {
        let pf = Pf {
            envs: Envs::Params {
                host: "H".to_owned(),
                port: Some("P".to_owned()),
                user: Some("U".to_owned()),
                password: Some("PW".to_owned()),
                database: None,
                scheme: Some("mssql"),
            },
            db_id: "mssql-branch".to_owned(),
        };
        let vars = HashMap::from([
            ("H".to_owned(), "10.0.0.5".to_owned()),
            ("P".to_owned(), "1433".to_owned()),
            ("U".to_owned(), "sa".to_owned()),
            ("PW".to_owned(), "pass".to_owned()),
        ]);

        let result = resolve_port_mappings([pf].into(), &vars);

        let key = (RemoteAddr::Ip("10.0.0.5".parse().unwrap()), 1433);
        let mapping = result.get(&key).unwrap();
        assert!(matches!(mapping.conn_info, ConnInfo::BuildMssql { .. }));
    }

    #[test]
    fn resolve_combined_host_port() {
        let pf = Pf {
            envs: Envs::Params {
                host: "SPANNER_EMULATOR_HOST".to_owned(),
                port: None,
                user: None,
                password: None,
                database: None,
                scheme: None,
            },
            db_id: "spanner-branch".to_owned(),
        };
        let vars = HashMap::from([(
            "SPANNER_EMULATOR_HOST".to_owned(),
            "10.0.0.9:9010".to_owned(),
        )]);

        let result = resolve_port_mappings([pf].into(), &vars);

        let key = (RemoteAddr::Ip("10.0.0.9".parse().unwrap()), 9010);
        let mapping = result.get(&key).unwrap();
        assert_eq!(mapping.db_id, "spanner-branch");
        assert!(matches!(mapping.conn_info, ConnInfo::HostPort));
    }
}
