use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt::Write as _,
    net::{IpAddr, SocketAddr},
};

use mirrord_config::{
    LayerConfig,
    feature::database_branches::{
        ConnectionParamsVars, ConnectionSource, DatabaseBranchConfig, DatabaseBranchesConfig,
        ParamSource, RedisBranchConfig, TargetEnvironmentVariableSource,
    },
};
use mirrord_intproxy::agent_conn::AgentConnection;
use mirrord_operator::client::database_branches::resolve_branch_id;
use mirrord_progress::NullProgress;
use mirrord_protocol::{ClientMessage, DaemonMessage, GetEnvVarsRequest, ResponseError};
use thiserror::Error;
use url::Url;

use crate::{
    config::RemoteAddr,
    db_branches::{Portforward, PortforwardSession, portforward_session_dir},
    ui::UiCliError,
};

#[derive(Debug, Error)]
pub(crate) enum SetupError {
    #[error("error response from agent: {0}")]
    AgentError(#[from] ResponseError),

    #[error("unexpected message received from agent: {0:?}")]
    UnexpectedAgentMessage(Box<DaemonMessage>),

    #[error("agent connection dropped unexpectedly")]
    AgentConnectionDropped,

    #[error("failed to attach DB branch port forward to the local daemon: {0}")]
    Daemon(#[from] UiCliError),

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
    /// Overrides for the connection string's query pairs: `Some` replaces the pair, `None`
    /// removes it. Needed because [`ConnInfo::ReplaceInUrl`] keeps the source URL's query
    /// verbatim, so a source-only param (pg's `sslmode=require`, mongo's IAM
    /// `authMechanism`/`authSource`) would be demanded from the branch too.
    query_overrides: BTreeMap<String, Option<String>>,
}

enum ConnInfo {
    /// The original URL with host:port to be replaced with local address.
    ReplaceInUrl {
        url: Url,
        query_overrides: BTreeMap<String, Option<String>>,
    },
    /// All params available to build a URL from scratch.
    BuildUrl {
        scheme: &'static str,
        user: String,
        password: String,
        database: Option<String>,
        query_overrides: BTreeMap<String, Option<String>>,
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

/// Applies `overrides` to the URL's query pairs: a `Some` value replaces the pair (appending
/// it when the URL does not carry the key yet), a `None` removes it. Existing occurrences of
/// an overridden key are dropped so a driver reading either the first or the last occurrence
/// sees the override. A no-op on an empty map, keeping untouched URLs byte-identical.
fn apply_query_overrides(url: &mut Url, overrides: &BTreeMap<String, Option<String>>) {
    if overrides.is_empty() {
        return;
    }
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| !overrides.contains_key(key.as_ref()))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let pairs: Vec<(&str, &str)> = kept
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .chain(
            overrides
                .iter()
                .filter_map(|(key, value)| Some((key.as_str(), value.as_deref()?))),
        )
        .collect();
    if pairs.is_empty() {
        url.set_query(None);
        return;
    }
    let mut serializer = url.query_pairs_mut();
    serializer.clear();
    for (key, value) in pairs {
        serializer.append_pair(key, value);
    }
}

impl ConnInfo {
    fn connection_string(&self, local: SocketAddr) -> String {
        match self {
            ConnInfo::ReplaceInUrl {
                url,
                query_overrides,
            } => {
                let mut url = url.clone();
                let host = match local.ip() {
                    IpAddr::V4(v4) => v4.to_string(),
                    IpAddr::V6(v6) => format!("[{v6}]"),
                };
                if url.set_host(Some(&host)).is_ok() && url.set_port(Some(local.port())).is_ok() {
                    apply_query_overrides(&mut url, query_overrides);
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
                query_overrides,
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
                apply_query_overrides(&mut url, query_overrides);
                // The branch root user lives in `admin`, so the built URL must always say so -
                // guaranteed after the overrides so a removal cannot strip it.
                if *scheme == "mongodb" && !url.query_pairs().any(|(key, _)| key == "authSource") {
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
        // than the shared `database.connection` block. It reuses the params path with no
        // separate port var: `port: None` tells the resolver to split the host var's value, and
        // the absent scheme makes it write the branch address back as a bare `host:port`.
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
                query_overrides: BTreeMap::new(),
            });
            continue;
        }

        let scheme = match branch {
            DatabaseBranchConfig::Clickhouse(_) => Some("clickhouse"),
            // CockroachDB is PostgreSQL-wire-compatible and the app keeps its PostgreSQL driver,
            // which rejects a `cockroachdb://` scheme, so the branch URL uses `postgresql`.
            DatabaseBranchConfig::Cockroachdb(_) => Some("postgresql"),
            DatabaseBranchConfig::Dynamodb(_) => Some("dynamodb"),
            DatabaseBranchConfig::Mongodb(_) => Some("mongodb"),
            DatabaseBranchConfig::Mysql(_) => Some("mysql"),
            DatabaseBranchConfig::Mariadb(_) => Some("mariadb"),
            DatabaseBranchConfig::Pg(_) => Some("postgresql"),
            DatabaseBranchConfig::Mssql(_) => Some("mssql"),
            DatabaseBranchConfig::Redis(db) => match &**db {
                RedisBranchConfig::Local(_) => continue,
                RedisBranchConfig::Remote(_) => Some("redis"),
            },
            // mirrord knows nothing about a generic branch's protocol, so the portforward
            // address is rendered as a bare `host:port` (no scheme), like Spanner's.
            DatabaseBranchConfig::Generic(_) => None,
            // An S3 branch is a bucket in the provider's cloud.
            // There's nothing to forward to.
            DatabaseBranchConfig::S3(_) => continue,
            DatabaseBranchConfig::Spanner(_) => unreachable!("handled above"),
        };
        let (Some(base), Some(database)) = (branch.base(), branch.database()) else {
            continue;
        };
        let envs = match &database.connection {
            ConnectionSource::Url { url } => match url {
                TargetEnvironmentVariableSource::Env { variable, .. }
                | TargetEnvironmentVariableSource::EnvFrom { variable, .. } => {
                    Envs::Url(variable.clone())
                }
                TargetEnvironmentVariableSource::Secret { .. }
                | TargetEnvironmentVariableSource::GcpSecretManager { .. }
                | TargetEnvironmentVariableSource::AwsSecretsManager { .. } => {
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
        let query_overrides = match branch {
            DatabaseBranchConfig::Pg(db) => db
                .query_params
                .iter()
                .map(|(key, value)| (key.clone(), Some(value.clone())))
                .collect(),
            // Under MONGODB-AWS the source URL carries IAM-only auth params the password-auth
            // branch pod cannot serve, and no userinfo to pair a SCRAM mechanism with (the
            // IAM role is the Mongo user, not the URL). There are no branch credentials to
            // substitute here (they travel only on the session channel), so the params are
            // removed: the string parses and connects, and the user supplies credentials.
            DatabaseBranchConfig::Mongodb(db) if db.iam_auth.is_some() => BTreeMap::from([
                ("authSource".to_owned(), None),
                ("authMechanism".to_owned(), None),
                ("authMechanismProperties".to_owned(), None),
            ]),
            _ => BTreeMap::new(),
        };
        portforwards.insert(Pf {
            envs,
            db_id,
            query_overrides,
        });
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
            let Pf {
                envs,
                db_id,
                query_overrides,
            } = pf;
            let (host, port, conn_info) = match envs {
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

                    (
                        host,
                        port,
                        ConnInfo::ReplaceInUrl {
                            url,
                            query_overrides,
                        },
                    )
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
                                    query_overrides,
                                }
                            })
                        })
                        .unwrap_or(ConnInfo::HostPort);

                    (remote_host, port_val, conn_info)
                }
            };
            Some(((host, port), PortMapping { db_id, conn_info }))
        })
        .collect()
}

fn remote_host(remote: &RemoteAddr) -> String {
    match remote {
        RemoteAddr::Ip(ip) => ip.to_string(),
        RemoteAddr::Hostname(host) => host.clone(),
    }
}

/// Resolves this session's DB branch connections and attaches them to daemon-owned forwards.
///
/// The intproxy reads the target's original database environment through `conn`, resolves each
/// configured branch endpoint, asks the local daemon to create or reuse a matching forward, and
/// rewrites the resulting connection strings with the daemon's local addresses. It publishes those
/// strings in the per-process file consumed by `mirrord db-branches connections`, but it does not
/// own or run the forwarding tasks.
///
/// `operator_session_id` identifies the remote operator session and is shown by the connections
/// command. `local_session_id` identifies this intproxy to the local daemon and acts as its claim
/// on each shared forward.
pub(super) async fn setup(
    config: &LayerConfig,
    conn: &mut AgentConnection,
    operator_session_id: u64,
    local_session_id: &str,
    key: &str,
    connect_info: mirrord_intproxy::agent_conn::AgentConnectInfo,
    daemon: &crate::ui::DaemonClient,
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
    let mut portforward_mappings = Vec::with_capacity(port_mappings.len());

    for ((remote, port), mapping) in port_mappings {
        let remote_host = remote_host(&remote);
        let response = daemon
            .attach_db_portforward(&crate::ui::db_portforwards::DbPortForwardAttachRequest {
                session_id: local_session_id.to_owned(),
                identity: crate::ui::db_portforwards::DbPortForwardIdentity {
                    kube_context: config.kube_context.clone(),
                    namespace: config.target.namespace.clone(),
                    db_id: mapping.db_id.clone(),
                    remote_host,
                    remote_port: port,
                },
                config: Box::new(config.clone()),
                connect_info: connect_info.clone(),
            })
            .await
            .map_err(SetupError::Daemon)?;

        portforward_mappings.push(Portforward {
            db_id: mapping.db_id,
            connection_string: mapping.conn_info.connection_string(response.local),
        });
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
            session_id: operator_session_id,
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
        std::future::pending::<()>().await;
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mirrord_config::feature::database_branches::{
        BranchBaseConfig, CockroachdbBranchConfig, ConnectionParamsConfig, ConnectionParamsVars,
        ConnectionSource, DatabaseBranchConfig, DatabaseBranchesConfig, DatabaseSourceConfig,
        MysqlBranchConfig, ParamSource, PgBranchConfig, TargetEnvironmentVariableSource,
    };

    use super::*;
    use crate::config::RemoteAddr;
    fn base(id: Option<&str>) -> BranchBaseConfig {
        BranchBaseConfig {
            id: id.map(str::to_owned),
            ttl_secs: Some(300),
            ..Default::default()
        }
    }

    fn database(connection: ConnectionSource) -> DatabaseSourceConfig {
        DatabaseSourceConfig {
            name: None,
            connection,
        }
    }

    fn mysql(id: Option<&str>, conn: ConnectionSource) -> DatabaseBranchConfig {
        DatabaseBranchConfig::Mysql(Box::new(MysqlBranchConfig {
            base: base(id),
            pod: Default::default(),
            database: database(conn),
            copy: Default::default(),
            iam_auth: None,
            migrations: None,
        }))
    }

    fn cockroachdb(id: Option<&str>, conn: ConnectionSource) -> DatabaseBranchConfig {
        DatabaseBranchConfig::Cockroachdb(Box::new(CockroachdbBranchConfig {
            base: base(id),
            pod: Default::default(),
            database: database(conn),
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
            query_overrides: BTreeMap::new(),
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
        assert!(matches!(mapping.conn_info, ConnInfo::ReplaceInUrl { .. }));
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
            query_overrides: BTreeMap::new(),
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
            query_overrides: BTreeMap::new(),
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
            query_overrides: BTreeMap::new(),
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

    // --- query param overrides ---

    fn pg(id: Option<&str>, conn: ConnectionSource) -> DatabaseBranchConfig {
        DatabaseBranchConfig::Pg(Box::new(PgBranchConfig {
            base: base(id),
            pod: Default::default(),
            database: database(conn),
            copy: Default::default(),
            connection_settings: Default::default(),
            query_params: BTreeMap::from([("sslmode".to_owned(), "disable".to_owned())]),
            iam_auth: None,
            migrations: None,
        }))
    }

    #[test]
    fn extract_pg_captures_query_params() {
        let config = DatabaseBranchesConfig(vec![pg(Some("db1"), url_env("DB_URL"))]);
        let result = extract_portforward_configs(&config, "key");

        let pf = result.into_iter().next().unwrap();
        assert_eq!(
            pf.query_overrides,
            BTreeMap::from([("sslmode".to_owned(), Some("disable".to_owned()))])
        );
    }

    /// The source URL's own `sslmode=require` describes the source's TLS setup; the branch
    /// pod serves no TLS, so the user's override must replace it in the local string.
    #[test]
    fn replace_in_url_applies_query_override() {
        let conn_info = ConnInfo::ReplaceInUrl {
            url: "postgresql://user:pass@db.example.com:5432/mydb?sslmode=require&app=x"
                .parse()
                .unwrap(),
            query_overrides: BTreeMap::from([("sslmode".to_owned(), Some("disable".to_owned()))]),
        };
        let local: SocketAddr = "127.0.0.1:5555".parse().unwrap();

        assert_eq!(
            conn_info.connection_string(local),
            "postgresql://user:pass@127.0.0.1:5555/mydb?app=x&sslmode=disable"
        );
    }

    #[test]
    fn replace_in_url_without_overrides_keeps_query_verbatim() {
        let conn_info = ConnInfo::ReplaceInUrl {
            url: "postgresql://user:pass@db.example.com:5432/mydb?sslmode=require"
                .parse()
                .unwrap(),
            query_overrides: BTreeMap::new(),
        };
        let local: SocketAddr = "127.0.0.1:5555".parse().unwrap();

        assert_eq!(
            conn_info.connection_string(local),
            "postgresql://user:pass@127.0.0.1:5555/mydb?sslmode=require"
        );
    }

    #[test]
    fn build_url_appends_query_overrides() {
        let conn_info = ConnInfo::BuildUrl {
            scheme: "postgresql",
            user: "admin".to_owned(),
            password: "secret".to_owned(),
            database: Some("mydb".to_owned()),
            query_overrides: BTreeMap::from([("sslmode".to_owned(), Some("disable".to_owned()))]),
        };
        let local: SocketAddr = "127.0.0.1:5555".parse().unwrap();

        assert_eq!(
            conn_info.connection_string(local),
            "postgresql://admin:secret@127.0.0.1:5555/mydb?sslmode=disable"
        );
    }

    /// A MONGODB-AWS branch keeps the source URL otherwise verbatim in the local string,
    /// so its IAM auth params must be removed - the branch pod cannot serve `MONGODB-AWS`
    /// or `$external`, and with no userinfo in the URL there are no credentials to pair a
    /// SCRAM mechanism with.
    #[test]
    fn extract_mongodb_iam_removes_auth_params() {
        use mirrord_config::feature::database_branches::{IamAuthConfig, MongodbBranchConfig};

        let with_iam = DatabaseBranchConfig::Mongodb(Box::new(MongodbBranchConfig {
            base: base(Some("db1")),
            pod: Default::default(),
            database: database(url_env("MONGO_URL")),
            copy: Default::default(),
            iam_auth: Some(IamAuthConfig::AwsRds {
                region: None,
                access_key_id: None,
                secret_access_key: None,
                session_token: None,
            }),
        }));
        let config = DatabaseBranchesConfig(vec![with_iam]);
        let pf = extract_portforward_configs(&config, "key")
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(
            pf.query_overrides,
            BTreeMap::from([
                ("authSource".to_owned(), None),
                ("authMechanism".to_owned(), None),
                ("authMechanismProperties".to_owned(), None),
            ])
        );

        let without_iam = DatabaseBranchConfig::Mongodb(Box::new(MongodbBranchConfig {
            base: base(Some("db2")),
            pod: Default::default(),
            database: database(url_env("MONGO_URL")),
            copy: Default::default(),
            iam_auth: None,
        }));
        let config = DatabaseBranchesConfig(vec![without_iam]);
        let pf = extract_portforward_configs(&config, "key")
            .into_iter()
            .next()
            .unwrap();
        assert!(pf.query_overrides.is_empty());
    }

    #[test]
    fn replace_in_url_removes_mongodb_aws_auth_params() {
        let removals = BTreeMap::from([
            ("authSource".to_owned(), None),
            ("authMechanism".to_owned(), None),
            ("authMechanismProperties".to_owned(), None),
        ]);
        let local: SocketAddr = "127.0.0.1:5555".parse().unwrap();

        let conn_info = ConnInfo::ReplaceInUrl {
            url: "mongodb://cluster0.example.mongodb.net:27017/appdb?authSource=%24external&authMechanism=MONGODB-AWS&retryWrites=true"
                .parse()
                .unwrap(),
            query_overrides: removals.clone(),
        };
        assert_eq!(
            conn_info.connection_string(local),
            "mongodb://127.0.0.1:5555/appdb?retryWrites=true"
        );

        // Removing every pair must drop the query entirely, not leave a dangling `?`.
        let conn_info = ConnInfo::ReplaceInUrl {
            url: "mongodb://cluster0.example.mongodb.net:27017/appdb?authSource=%24external&authMechanism=MONGODB-AWS"
                .parse()
                .unwrap(),
            query_overrides: removals,
        };
        assert_eq!(
            conn_info.connection_string(local),
            "mongodb://127.0.0.1:5555/appdb"
        );
    }

    #[test]
    fn build_url_mongodb_auth_source_survives_overrides() {
        let local: SocketAddr = "127.0.0.1:5555".parse().unwrap();

        let conn_info = ConnInfo::BuildUrl {
            scheme: "mongodb",
            user: "admin".to_owned(),
            password: "secret".to_owned(),
            database: None,
            query_overrides: BTreeMap::from([("retryWrites".to_owned(), Some("false".to_owned()))]),
        };
        assert_eq!(
            conn_info.connection_string(local),
            "mongodb://admin:secret@127.0.0.1:5555?retryWrites=false&authSource=admin"
        );

        // Even an `authSource` removal cannot strip it from a built mongo URL: the branch
        // root user lives in `admin`, and the built URL carries its credentials.
        let conn_info = ConnInfo::BuildUrl {
            scheme: "mongodb",
            user: "admin".to_owned(),
            password: "secret".to_owned(),
            database: None,
            query_overrides: BTreeMap::from([("authSource".to_owned(), None)]),
        };
        assert_eq!(
            conn_info.connection_string(local),
            "mongodb://admin:secret@127.0.0.1:5555?authSource=admin"
        );
    }
}
