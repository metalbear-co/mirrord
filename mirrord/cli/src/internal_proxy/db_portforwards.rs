use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::Arc,
};

use mirrord_config::feature::database_branches::{
    ConnectionParamsVars, ConnectionSource, DatabaseBranchConfig, DatabaseBranchesConfig,
    ParamSource, TargetEnvironmentVariableSource,
};
use mirrord_intproxy::agent_conn::AgentConnection;
use mirrord_operator::client::database_branches::resolve_branch_id;
use mirrord_progress::NullProgress;
use mirrord_protocol::{
    ClientMessage, DaemonMessage, GetEnvVarsRequest, ResponseError,
    outgoing::tcp::DaemonTcpOutgoing,
};
use thiserror::Error;
use url::Url;

use crate::{
    config::RemoteAddr,
    db_branches::{Portforward, PortforwardSession, portforward_session_dir},
    port_forward,
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
                        .unwrap_or_else(|_| RemoteAddr::Hostname(host.to_string()));

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
    config: &DatabaseBranchesConfig,
    conn: &mut AgentConnection,
    session_id: u64,
    key: &str,
) -> Result<(), SetupError> {
    let portforwards = extract_portforward_configs(config, key);

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

    let connections_state = Arc::new(port_forward::ConnectionsState::default());
    let connections_state_2 = Arc::clone(&connections_state);

    let pf_rx = conn.connection.split_incoming(64, move |inc| {
        let DaemonMessage::TcpOutgoing(tcp) = inc else {
            return false;
        };
        match tcp {
            DaemonTcpOutgoing::Connect(_) => false,
            DaemonTcpOutgoing::Read(read) => match read {
                Ok(read) => connections_state
                    .ongoing
                    .lock()
                    .unwrap()
                    .contains(&read.connection_id),
                Err(err) => {
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
    .await?;

    let portforward_mappings: Vec<_> = portforwarder
        .listeners()
        .filter_map(|(local, remote)| {
            let mapping = port_mappings.get(remote)?;
            Some(Portforward {
                db_id: mapping.db_id.clone(),
                connection_string: mapping.conn_info.connection_string(local),
            })
        })
        .collect();

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
        let _pf_guard = pf_guard;
        if let Err(err) = portforwarder.run().await {
            tracing::error!(?err, "DB branch portforwarding failed");
        }
    });

    Ok(())
}
