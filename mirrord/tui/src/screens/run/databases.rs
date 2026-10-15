//! Finds the databases a target uses, and how it reaches each one, from its pod spec alone.
//!
//! A target can use several databases, so everything here works on lists. For each database the
//! result says which engine it is, where its connection details come from (one URL variable, or
//! separate host/port/user/password variables, each from a plain value, a Secret, a ConfigMap or
//! `envFrom`), and whether the pod signs in with a cloud identity instead of a password. That is
//! exactly what a `feature.db_branches` entry needs, so a detected database becomes a branch with
//! no typing.
//!
//! Secret values are never read: the pod spec only says which Secret and key a variable comes
//! from, which is all the branch config needs. The operator reads the values itself.

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// Database engines mirrord can branch, with the names the config uses for them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Engine {
    Postgres,
    Mysql,
    Mariadb,
    Mongodb,
    Mssql,
    Clickhouse,
    Redis,
}

impl Engine {
    /// The `type` of a `feature.db_branches` entry.
    pub fn config_type(self) -> &'static str {
        match self {
            Self::Postgres => "pg",
            Self::Mysql => "mysql",
            Self::Mariadb => "mariadb",
            Self::Mongodb => "mongodb",
            Self::Mssql => "mssql",
            Self::Clickhouse => "clickhouse",
            Self::Redis => "redis",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::Mysql => "mysql",
            Self::Mariadb => "mariadb",
            Self::Mongodb => "mongodb",
            Self::Mssql => "sql server",
            Self::Clickhouse => "clickhouse",
            Self::Redis => "redis",
        }
    }

    /// Whether the Run view can write a branch for it. Redis branches take settings the pod spec
    /// does not reveal, so a detected Redis is shown but left to the Config view.
    pub fn branchable(self) -> bool {
        self != Self::Redis
    }

    /// Whether `iam_auth` with the given cloud is accepted for this engine.
    fn takes_iam(self, cloud: Cloud) -> bool {
        matches!(
            (self, cloud),
            (Self::Postgres | Self::Mysql | Self::Mariadb, _) | (Self::Mongodb, Cloud::Aws)
        )
    }

    /// The engine a URL scheme names, `jdbc:` prefix allowed.
    fn from_scheme(url: &str) -> Option<Self> {
        let url = url.to_ascii_lowercase();
        let url = url.strip_prefix("jdbc:").unwrap_or(&url);
        let scheme = url.split("://").next()?;
        Some(match scheme {
            "postgres" | "postgresql" => Self::Postgres,
            "mysql" => Self::Mysql,
            "mariadb" => Self::Mariadb,
            "mongodb" | "mongodb+srv" => Self::Mongodb,
            "sqlserver" | "mssql" => Self::Mssql,
            "clickhouse" => Self::Clickhouse,
            "redis" | "rediss" => Self::Redis,
            _ => return None,
        })
    }

    /// The engine a variable or prefix name points to, such as `PG`, `POSTGRES_` or `MONGO`.
    fn from_name(name: &str) -> Option<Self> {
        let name = name.to_ascii_uppercase();
        let has = |word: &str| name.split(['_', '-']).any(|part| part.starts_with(word));
        if has("PG") || has("POSTGRES") {
            Some(Self::Postgres)
        } else if has("MARIA") {
            Some(Self::Mariadb)
        } else if has("MYSQL") {
            Some(Self::Mysql)
        } else if has("MONGO") {
            Some(Self::Mongodb)
        } else if has("MSSQL") || has("SQLSERVER") {
            Some(Self::Mssql)
        } else if has("CLICKHOUSE") {
            Some(Self::Clickhouse)
        } else if has("REDIS") {
            Some(Self::Redis)
        } else {
            None
        }
    }

    /// The engine conventionally listening on `port`.
    fn from_port(port: &str) -> Option<Self> {
        Some(match port.trim() {
            "5432" | "26257" => Self::Postgres,
            "3306" => Self::Mysql,
            "27017" => Self::Mongodb,
            "1433" => Self::Mssql,
            "9000" | "8123" => Self::Clickhouse,
            "6379" => Self::Redis,
            _ => return None,
        })
    }
}

/// Where an environment variable's value comes from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    Literal(String),
    Secret {
        name: String,
        key: String,
    },
    ConfigMap {
        name: String,
        key: String,
    },
    /// A key of a Secret or ConfigMap the container loads whole with `envFrom`.
    EnvFrom {
        kind: &'static str,
        name: String,
    },
    /// Anything else the pod computes (`fieldRef`, `resourceFieldRef`).
    Other,
}

impl Source {
    fn literal(&self) -> Option<&str> {
        match self {
            Self::Literal(value) => Some(value),
            _ => None,
        }
    }

    /// Where the value comes from, for the card. A literal value is never shown, since it may
    /// hold a password.
    fn describe(&self) -> String {
        match self {
            Self::Literal(_) => "set in the pod spec".to_owned(),
            Self::Secret { name, key } => format!("secret {name}/{key}"),
            Self::ConfigMap { name, key } => format!("configmap {name}/{key}"),
            Self::EnvFrom { kind, name } => format!("{kind} {name} via envFrom"),
            Self::Other => "computed by the pod".to_owned(),
        }
    }
}

/// One environment variable of a container.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvVar {
    pub name: String,
    pub source: Source,
}

/// A cloud the pod has an identity in, which a database may accept instead of a password.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cloud {
    Aws,
    Gcp,
}

/// How the pod signs in to its cloud, when it does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Identity {
    pub cloud: Cloud,
    /// What gave it away, such as `the service account's IRSA role`.
    pub reason: String,
}

/// What the pod spec says, as far as finding databases goes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PodFacts {
    /// The variables of the container being targeted (or of every container, when none is).
    pub env: Vec<EnvVar>,
    /// Annotations of the pod's service account.
    pub service_account_annotations: BTreeMap<String, String>,
    /// Names of every container in the pod, sidecars included.
    pub containers: Vec<String>,
}

/// How a database's connection details are found in the pod.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Connection {
    /// One variable holds the whole URL.
    Url { variable: String, source: Source },
    /// Separate variables, by the part each one holds (`host`, `port`, `user`, `password`,
    /// `database`).
    Params(BTreeMap<&'static str, EnvVar>),
}

/// A database the target uses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Database {
    pub engine: Engine,
    pub connection: Connection,
    /// The cloud identity the branch signs in with, when the pod uses one and no password was
    /// found.
    pub iam: Option<Identity>,
    /// How the engine was told, such as `postgres:// in its value`.
    pub reason: String,
}

impl Database {
    /// A short name for the card and the picker: the URL variable, or the shared prefix of the
    /// parameter variables.
    pub fn label(&self) -> String {
        match &self.connection {
            Connection::Url { variable, .. } => variable.clone(),
            Connection::Params(parts) => parts
                .get("host")
                .map(|host| {
                    host.name
                        .trim_end_matches("HOST")
                        .trim_end_matches('_')
                        .to_owned()
                })
                .filter(|prefix| !prefix.is_empty())
                .unwrap_or_else(|| "database".to_owned()),
        }
    }

    /// Where its credentials come from, in a few words.
    pub fn credentials(&self) -> String {
        let source = match &self.connection {
            Connection::Url { source, .. } => format!("url from {}", source.describe()),
            Connection::Params(parts) => match parts.get("password") {
                Some(password) => format!("password from {}", password.source.describe()),
                None => "no password variable".to_owned(),
            },
        };
        match &self.iam {
            Some(identity) => format!("{source}; signs in with {}", identity.reason),
            None => source,
        }
    }

    /// The `feature.db_branches` entry that branches it.
    pub fn branch_config(&self) -> Value {
        let connection = match &self.connection {
            Connection::Url { variable, source } => {
                let kind = match source {
                    Source::EnvFrom { .. } => "env_from",
                    _ => "env",
                };
                json!({ "url": { "type": kind, "variable": variable } })
            }
            // The operator works out on its own whether each variable comes from `env` or
            // `envFrom`, so params need no `type`.
            Connection::Params(parts) => {
                let params: serde_json::Map<String, Value> = parts
                    .iter()
                    .map(|(part, var)| ((*part).to_owned(), Value::String(var.name.clone())))
                    .collect();
                json!({ "params": params })
            }
        };

        let mut entry = json!({ "type": self.engine.config_type(), "connection": connection });
        if let (Some(identity), Value::Object(map)) = (&self.iam, &mut entry) {
            let iam = match identity.cloud {
                Cloud::Aws => json!({ "type": "aws_rds" }),
                Cloud::Gcp => json!({ "type": "gcp_cloud_sql" }),
            };
            map.insert("iam_auth".to_owned(), iam);
        }
        entry
    }
}

/// Variable name endings that hold a whole connection URL.
const URL_SUFFIXES: &[&str] = &["_URL", "_URI", "_DSN", "_CONNECTION_STRING"];

/// Variable name endings for the separate connection parts, by the part they hold.
const PART_SUFFIXES: &[(&str, &[&str])] = &[
    ("host", &["HOST", "HOSTNAME", "SERVER"]),
    ("port", &["PORT"]),
    ("user", &["USER", "USERNAME"]),
    ("password", &["PASSWORD", "PASS", "PWD"]),
    ("database", &["NAME", "DB", "DATABASE", "DBNAME"]),
];

/// Every database the pod's variables point to, in the order found: URL variables first, then
/// groups of host/port/user/password variables sharing a prefix (`DB_HOST`, `DB_PORT` ...,
/// or `PGHOST`, `PGPORT` ...).
pub fn detect(facts: &PodFacts) -> Vec<Database> {
    let identity = identity(facts);
    let mut databases = Vec::new();
    let mut covered_prefixes: Vec<String> = Vec::new();

    for var in &facts.env {
        let upper = var.name.to_ascii_uppercase();
        let Some(suffix) = URL_SUFFIXES.iter().find(|suffix| upper.ends_with(*suffix)) else {
            continue;
        };
        let prefix = upper.trim_end_matches(suffix).to_owned();

        let (engine, reason) = match var.source.literal().and_then(Engine::from_scheme) {
            Some(engine) => (engine, format!("{}:// in its value", engine.name())),
            None => match Engine::from_name(&upper) {
                Some(engine) => (engine, format!("{} in its name", engine.name())),
                // Listed by `unidentified` instead: a branch needs its engine up front.
                None => continue,
            },
        };

        // A literal URL with credentials in it signs in with a password; one without, on a pod
        // with a cloud identity, most likely signs in with that identity.
        let has_password = var.source.literal().is_none_or(|url| {
            url.split("://")
                .nth(1)
                .is_some_and(|rest| rest.contains('@'))
        });
        let iam = identity
            .clone()
            .filter(|identity| !has_password && engine.takes_iam(identity.cloud));

        covered_prefixes.push(prefix);
        databases.push(Database {
            engine,
            connection: Connection::Url {
                variable: var.name.clone(),
                source: var.source.clone(),
            },
            iam,
            reason,
        });
    }

    for (prefix, parts) in part_groups(&facts.env) {
        if covered_prefixes.contains(&prefix) || !parts.contains_key("host") {
            continue;
        }
        let port = parts
            .get("port")
            .and_then(|port| port.source.literal())
            .and_then(Engine::from_port);
        let (engine, reason) = match (Engine::from_name(&prefix), port) {
            (Some(engine), _) => (engine, format!("{} in the variable names", engine.name())),
            (None, Some(engine)) => (
                engine,
                format!("port {} is {}'s", port_of(&parts), engine.name()),
            ),
            (None, None) => continue,
        };
        let iam = identity
            .clone()
            .filter(|identity| !parts.contains_key("password") && engine.takes_iam(identity.cloud));
        databases.push(Database {
            engine,
            connection: Connection::Params(parts),
            iam,
            reason,
        });
    }

    databases
}

fn port_of(parts: &BTreeMap<&'static str, EnvVar>) -> String {
    parts
        .get("port")
        .and_then(|port| port.source.literal())
        .unwrap_or_default()
        .to_owned()
}

/// URL variables that look like a database's (`DATABASE_URL`, `ORDERS_DB_URL`) but whose engine
/// neither the name nor a plain value gives away, such as a `DATABASE_URL` from a Secret. The
/// card lists them so the user knows they were seen.
pub fn unidentified(facts: &PodFacts) -> Vec<String> {
    facts
        .env
        .iter()
        .filter(|var| {
            let upper = var.name.to_ascii_uppercase();
            URL_SUFFIXES.iter().any(|suffix| upper.ends_with(suffix))
                && (upper.contains("DATABASE") || upper.split('_').any(|part| part == "DB"))
                && Engine::from_name(&upper).is_none()
                && var.source.literal().and_then(Engine::from_scheme).is_none()
        })
        .map(|var| var.name.clone())
        .collect()
}

/// Groups the variables that name a connection part by the prefix they share.
fn part_groups(env: &[EnvVar]) -> BTreeMap<String, BTreeMap<&'static str, EnvVar>> {
    let mut groups: BTreeMap<String, BTreeMap<&'static str, EnvVar>> = BTreeMap::new();
    for var in env {
        let upper = var.name.to_ascii_uppercase();
        for (part, suffixes) in PART_SUFFIXES {
            // Longest suffix first, so `USERNAME` is not read as `NAME`.
            let mut sorted: Vec<&&str> = suffixes.iter().collect();
            sorted.sort_by_key(|suffix| std::cmp::Reverse(suffix.len()));
            let Some(suffix) = sorted.into_iter().find(|suffix| upper.ends_with(**suffix)) else {
                continue;
            };
            let prefix = upper
                .trim_end_matches(*suffix)
                .trim_end_matches('_')
                .to_owned();
            // `HOST` alone, or `USERNAME` mistaken for `USER` + `NAME`, is not a database group.
            if prefix.is_empty() || (*part == "database" && prefix.ends_with("USER")) {
                continue;
            }
            groups
                .entry(prefix)
                .or_default()
                .entry(part)
                .or_insert_with(|| var.clone());
            break;
        }
    }
    groups
}

/// The cloud identity the pod signs in with, if any. First match wins:
/// - an IRSA role on the service account, or the web-identity variables EKS injects
/// - static AWS keys in the environment
/// - a GKE Workload Identity binding on the service account
/// - a Cloud SQL proxy sidecar
pub fn identity(facts: &PodFacts) -> Option<Identity> {
    let annotations = &facts.service_account_annotations;
    let has_env = |name: &str| facts.env.iter().any(|var| var.name == name);

    if annotations.contains_key("eks.amazonaws.com/role-arn")
        || has_env("AWS_ROLE_ARN")
        || has_env("AWS_WEB_IDENTITY_TOKEN_FILE")
    {
        return Some(Identity {
            cloud: Cloud::Aws,
            reason: "the pod's AWS role (IRSA)".to_owned(),
        });
    }
    if has_env("AWS_ACCESS_KEY_ID") {
        return Some(Identity {
            cloud: Cloud::Aws,
            reason: "the pod's AWS keys".to_owned(),
        });
    }
    if annotations.contains_key("iam.gke.io/gcp-service-account") {
        return Some(Identity {
            cloud: Cloud::Gcp,
            reason: "the pod's Google identity (Workload Identity)".to_owned(),
        });
    }
    if facts.containers.iter().any(|container| {
        container.contains("cloud-sql-proxy") || container.contains("cloudsql-proxy")
    }) {
        return Some(Identity {
            cloud: Cloud::Gcp,
            reason: "the Cloud SQL proxy sidecar".to_owned(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{Connection, Engine, EnvVar, PodFacts, Source, detect, unidentified};

    fn literal(name: &str, value: &str) -> EnvVar {
        EnvVar {
            name: name.to_owned(),
            source: Source::Literal(value.to_owned()),
        }
    }

    fn secret(name: &str, secret: &str, key: &str) -> EnvVar {
        EnvVar {
            name: name.to_owned(),
            source: Source::Secret {
                name: secret.to_owned(),
                key: key.to_owned(),
            },
        }
    }

    #[test]
    fn finds_every_database_and_where_its_credentials_come_from() {
        let facts = PodFacts {
            env: vec![
                literal(
                    "ORDERS_DATABASE_URL",
                    "postgres://app:pw@orders-db:5432/orders",
                ),
                secret("MONGO_URL", "app-secrets", "mongo-url"),
                literal("USERS_DB_HOST", "users-db"),
                literal("USERS_DB_PORT", "3306"),
                literal("USERS_DB_USER", "app"),
                secret("USERS_DB_PASSWORD", "users-db", "password"),
                literal("USERS_DB_NAME", "users"),
                literal("LOG_LEVEL", "debug"),
            ],
            ..PodFacts::default()
        };

        let found = detect(&facts);
        let engines: Vec<Engine> = found.iter().map(|db| db.engine).collect();
        assert_eq!(engines, [Engine::Postgres, Engine::Mongodb, Engine::Mysql]);

        assert_eq!(found[0].reason, "postgres:// in its value");
        assert_eq!(
            found[1].credentials(),
            "url from secret app-secrets/mongo-url"
        );
        assert_eq!(found[2].label(), "USERS_DB");
        assert_eq!(
            found[2].credentials(),
            "password from secret users-db/password"
        );
        assert!(matches!(&found[2].connection, Connection::Params(parts) if parts.len() == 5));

        assert_eq!(
            found[0].branch_config(),
            json!({
                "type": "pg",
                "connection": { "url": { "type": "env", "variable": "ORDERS_DATABASE_URL" } }
            })
        );
        assert_eq!(
            found[2].branch_config()["connection"]["params"]["password"],
            json!("USERS_DB_PASSWORD")
        );
    }

    #[test]
    fn libpq_style_variables_and_an_irsa_role_without_a_password_use_iam() {
        let facts = PodFacts {
            env: vec![
                literal("PGHOST", "orders.cluster-abc.rds.amazonaws.com"),
                literal("PGUSER", "app"),
                literal("PGDATABASE", "orders"),
            ],
            service_account_annotations: BTreeMap::from([(
                "eks.amazonaws.com/role-arn".to_owned(),
                "arn:aws:iam::1:role/orders".to_owned(),
            )]),
            ..PodFacts::default()
        };

        let found = detect(&facts);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].engine, Engine::Postgres);
        assert_eq!(
            found[0].branch_config()["iam_auth"],
            json!({ "type": "aws_rds" })
        );
        assert!(
            found[0].credentials().contains("IRSA"),
            "{}",
            found[0].credentials()
        );
    }

    #[test]
    fn a_password_keeps_iam_off_and_unknown_engines_are_skipped() {
        let facts = PodFacts {
            env: vec![
                secret("PGPASSWORD", "db", "password"),
                literal("PGHOST", "db"),
                // No engine in the name, no port: nothing to branch.
                literal("CACHE_HOST", "somewhere"),
            ],
            service_account_annotations: BTreeMap::from([(
                "iam.gke.io/gcp-service-account".to_owned(),
                "app@project.iam.gserviceaccount.com".to_owned(),
            )]),
            ..PodFacts::default()
        };

        let found = detect(&facts);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].iam, None);
    }

    #[test]
    fn a_database_url_from_a_secret_is_listed_as_unidentified() {
        let facts = PodFacts {
            env: vec![
                secret("DATABASE_URL", "app", "url"),
                literal("API_URL", "https://api"),
            ],
            ..PodFacts::default()
        };
        assert!(detect(&facts).is_empty());
        assert_eq!(unidentified(&facts), ["DATABASE_URL"]);
    }
}
