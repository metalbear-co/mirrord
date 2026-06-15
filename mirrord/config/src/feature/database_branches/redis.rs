use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::{
    config::ConfigError, container::ContainerRuntime,
    feature::database_branches::DatabaseBranchBaseConfig,
};

/// When configuring a branch for Redis, set `type` to `redis`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize)]
#[schemars(transform = Self::transform_schema)]
#[serde(tag = "location", rename_all = "lowercase", deny_unknown_fields)]
pub enum RedisBranchConfig {
    Local(LocalRedisBranchConfig),
    Remote(RemoteRedisBranchConfig),
}

impl RedisBranchConfig {
    fn transform_schema(schema: &mut Schema) {
        const DESCRIPTION: &str = r#"#### feature.db_branches[].location (type: redis) {#feature-db_branches-redis-location}
Where the Redis instance should run.
- `local`: Spawns a local instance.
- `remote`: Uses a remote instance (default)."#;

        let variants = schema
            .get_mut("oneOf")
            .and_then(|v| v.as_array_mut())
            .unwrap();

        for variant in variants {
            let variant = variant.as_object_mut().unwrap();

            variant
                .get_mut("properties")
                .and_then(|properties| properties.get_mut("location"))
                .and_then(|location| location.as_object_mut())
                .unwrap()
                .insert("description".into(), DESCRIPTION.into());

            let required = variant.get_mut("required").unwrap().as_array_mut().unwrap();

            required.retain(|field| field != "location");

            if required.is_empty() {
                variant.remove("required");
            }
        }
    }

    #[cfg(test)]
    fn into_local(self) -> LocalRedisBranchConfig {
        match self {
            RedisBranchConfig::Local(config) => config,
            RedisBranchConfig::Remote(config) => panic!("expected local, got remote: {config:?}"),
        }
    }
}

impl<'de> Deserialize<'de> for RedisBranchConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "location", rename_all = "lowercase", deny_unknown_fields)]
        enum Tagged {
            Local(LocalRedisBranchConfig),
            Remote(RemoteRedisBranchConfig),
        }

        let mut value = Value::deserialize(deserializer)?;

        if let Some(object) = value.as_object_mut() {
            object
                .entry("location")
                .or_insert_with(|| Value::String("remote".into()));
        }

        Ok(
            match Tagged::deserialize(value).map_err(serde::de::Error::custom)? {
                Tagged::Local(config) => Self::Local(config),
                Tagged::Remote(config) => Self::Remote(config),
            },
        )
    }
}

/// Configuration for a local Redis branch.
///
/// Example with URL-based connection:
/// ```json
/// {
///   "type": "redis",
///   "location": "local",
///   "connection": {
///     "url": {
///       "type": "env",
///       "variable": "REDIS_URL"
///     }
///   }
/// }
/// ```
///
/// Example with separated settings:
/// ```json
/// {
///   "type": "redis",
///   "location": "local",
///   "connection": {
///     "host": { "type": "env", "variable": "REDIS_HOST" },
///     "port": 6379,
///     "password": { "type": "env", "variable": "REDIS_PASSWORD" }
///   }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalRedisBranchConfig {
    /// #### feature.db_branches[].id (type: redis) {#feature-db_branches-redis-id}
    ///
    /// Optional unique identifier for reusing branches across sessions.
    #[serde(default)]
    pub id: Option<String>,

    /// #### feature.db_branches[].connection (type: redis) {#feature-db_branches-redis-connection}
    ///
    /// Connection configuration for the Redis instance.
    #[serde(default)]
    pub connection: RedisConnectionConfig,

    /// #### feature.db_branches[].local (type: redis) {#feature-db_branches-redis-local}
    ///
    /// Local Redis runtime configuration.
    #[serde(default)]
    pub local: RedisLocalConfig,
}

/// Configuration for a remote Redis branch.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteRedisBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    /// #### feature.db_branches[].copy (type: redis) {#feature-db_branches-redis-copy}
    ///
    /// How a Redis branch is seeded from its source.
    #[serde(default)]
    pub copy: RedisBranchCopyConfig,
}

impl RemoteRedisBranchConfig {
    pub fn verify(&self) -> Result<(), ConfigError> {
        self.base.verify()?;

        if let Some(name) = self.base.name.as_deref() {
            name.parse::<u32>()
                .map_err(|error| ConfigError::InvalidValue {
                    name: "feature.db_branches[].name".into(),
                    provided: name.to_owned(),
                    error: Box::new(error),
                })?;
        }

        Ok(())
    }
}

/// How a Redis branch is seeded from its source.
///
/// - `empty` (default): Start a fresh, empty instance.
/// - `all`: Copy keys from the source instance. Optional `patterns` are `SCAN MATCH` globs limiting
///   which keys are copied; omitting them copies the whole keyspace.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum RedisBranchCopyConfig {
    #[default]
    Empty,
    All {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        patterns: Option<Vec<String>>,
    },
}

/// Supports either a complete URL or separated connection parameters.
/// If both are provided, `url` takes precedence.
///
/// The following fields can be sourced via remote environment variable:
/// - url
/// - host
/// - password
/// - username
///
/// Example:
/// ```json
/// "connection": {
///     "host": { "type": "env", "variable": "REDIS_HOST" },
///     "port": 6379,
///     "password": { "type": "env", "variable": "REDIS_PASSWORD" }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RedisConnectionConfig {
    /// ##### feature.db_branches[].connection.url (type: redis)
    ///
    /// Complete Redis URL (e.g., `redis://user:pass@host:6379/0`).
    /// Can be sourced from an environment variable.
    #[serde(default)]
    pub url: Option<RedisValueSource>,

    /// ##### feature.db_branches[].connection.host (type: redis)
    ///
    /// Redis host/hostname.
    /// Can be sourced from an environment variable.
    #[serde(default)]
    pub host: Option<RedisValueSource>,

    /// ##### feature.db_branches[].connection.port (type: redis)
    ///
    /// Redis port (default: 6379).
    #[serde(default)]
    pub port: Option<u16>,

    /// ##### feature.db_branches[].connection.password (type: redis)
    ///
    /// Redis password for authentication.
    /// Can be sourced from an environment variable.
    #[serde(default)]
    pub password: Option<RedisValueSource>,

    /// ##### feature.db_branches[].connection.username (type: redis)
    ///
    /// Redis username (Redis 6+ ACL).
    /// Can be sourced from an environment variable.
    #[serde(default)]
    pub username: Option<RedisValueSource>,

    /// ##### feature.db_branches[].connection.database (type: redis)
    ///
    /// Redis database number (default: 0).
    #[serde(default)]
    pub database: Option<u16>,

    /// ##### feature.db_branches[].connection.tls (type: redis)
    ///
    /// Enable TLS/SSL connection.
    #[serde(default)]
    pub tls: Option<bool>,
}

impl RedisConnectionConfig {
    /// Get the effective port, defaulting to 6379.
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(6379)
    }

    /// Check if URL-based connection is configured.
    pub fn has_url(&self) -> bool {
        self.url.is_some()
    }

    /// Get the environment variable name for the connection override.
    /// Returns the URL variable if set, otherwise the host variable.
    pub fn override_variable(&self) -> Option<&str> {
        self.url
            .as_ref()
            .and_then(|u| u.env_variable())
            .or_else(|| self.host.as_ref().and_then(|h| h.env_variable()))
    }

    pub(crate) fn collect_env_keys<'a>(&'a self, out: &mut Vec<&'a str>) {
        for source in [&self.url, &self.host, &self.password, &self.username] {
            if let Some(name) = source.as_ref().and_then(RedisValueSource::env_variable) {
                out.push(name);
            }
        }
    }
}

/// <!--${internal}-->
/// Source for a Redis configuration value.
///
/// Values can be specified directly or sourced from environment variables.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum RedisValueSource {
    Direct(String),
    Env(RedisEnvSource),
}

impl RedisValueSource {
    /// Get the environment variable name if this is an env source.
    pub fn env_variable(&self) -> Option<&str> {
        match self {
            Self::Env(env) => Some(&env.variable),
            Self::Direct(_) => None,
        }
    }
}

/// <!--${internal}-->
/// Environment variable source for Redis values.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedisEnvSource {
    #[serde(rename = "type")]
    pub source_type: RedisEnvSourceType,

    pub variable: String,

    #[serde(default)]
    pub container: Option<String>,
}

/// <!--${internal}-->
/// Type marker for environment variable sources.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RedisEnvSourceType {
    Env,
}

/// Configuration for local Redis runtime.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedisLocalConfig {
    /// ##### feature.db_branches[].local.port (type: redis)
    ///
    /// Local port to bind Redis to (default: 6379).
    #[serde(default = "default_local_port")]
    pub port: u16,

    /// ##### feature.db_branches[].local.version (type: redis)
    ///
    /// Redis version/tag to use (default: "7-alpine").
    /// Used as the container image tag.
    #[serde(default = "default_redis_version")]
    pub version: String,

    /// ##### feature.db_branches[].local.runtime (type: redis)
    ///
    /// Runtime backend for local Redis: `container`, `redis_server`, or `auto`.
    #[serde(default)]
    pub runtime: RedisRuntime,

    /// ##### feature.db_branches[].local.container_runtime (type: redis)
    ///
    /// Which container runtime to use (Docker, Podman, or nerdctl).
    /// Only applies when `runtime` is `container` or `auto`.
    #[serde(default)]
    pub container_runtime: ContainerRuntime,

    /// ##### feature.db_branches[].local.container_command (type: redis)
    ///
    /// Custom path to the container command.
    /// If not provided, uses the runtime name from PATH (e.g., "docker").
    /// Example: `/usr/local/bin/docker` or `/home/user/.local/bin/podman`
    #[serde(default)]
    pub container_command: Option<String>,

    /// ##### feature.db_branches[].local.server_command (type: redis)
    ///
    /// Custom path to the redis-server binary.
    /// If not provided, uses "redis-server" from PATH.
    /// Example: `/opt/redis/bin/redis-server`
    #[serde(default)]
    pub server_command: Option<String>,

    /// ##### feature.db_branches[].local.options (type: redis)
    ///
    /// Additional Redis configuration options.
    #[serde(default)]
    pub options: RedisOptions,
}

impl Default for RedisLocalConfig {
    fn default() -> Self {
        Self {
            port: default_local_port(),
            version: default_redis_version(),
            runtime: RedisRuntime::default(),
            container_runtime: ContainerRuntime::default(),
            container_command: None,
            server_command: None,
            options: RedisOptions::default(),
        }
    }
}

/// For container-based runtimes, mirrord spawns the Redis image in a container.
/// For `redis_server`, it runs the native binary directly.
///
/// Backends:
/// - `container` (default) - Uses a container runtime (Docker/Podman/nerdctl), configured via
///   `container_runtime`.
/// - `redis_server` - Uses native redis-server binary
/// - `auto` - Tries container first, falls back to redis-server
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RedisRuntime {
    #[default]
    Container,
    RedisServer,
    Auto,
}

/// Example:
/// ```json
/// {
///   "args": ["--maxmemory", "256mb", "--appendonly", "yes"]
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RedisOptions {
    /// Raw arguments passed directly to redis-server or as Docker CMD args.
    /// Use standard Redis config syntax (e.g., "--maxmemory 256mb").
    #[serde(default)]
    pub args: Vec<String>,
}

fn default_local_port() -> u16 {
    6379
}

fn default_redis_version() -> String {
    "7-alpine".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_based_config() {
        let json = r#"{
            "location": "local",
            "connection": {
                "url": {
                    "type": "env",
                    "variable": "REDIS_URL"
                }
            }
        }"#;

        let config = serde_json::from_str::<RedisBranchConfig>(json)
            .unwrap()
            .into_local();

        assert!(config.connection.has_url());
        assert_eq!(config.connection.override_variable(), Some("REDIS_URL"));
    }

    #[test]
    fn test_separated_settings_config() {
        let json = r#"{
            "location": "local",
            "connection": {
                "host": {
                    "type": "env",
                    "variable": "REDIS_HOST"
                },
                "port": 6380,
                "password": "secret123"
            }
        }"#;

        let config = serde_json::from_str::<RedisBranchConfig>(json)
            .unwrap()
            .into_local();

        assert!(!config.connection.has_url());
        assert_eq!(config.connection.effective_port(), 6380);
        assert_eq!(config.connection.override_variable(), Some("REDIS_HOST"));
    }

    #[test]
    fn test_local_runtime_config() {
        let json = r#"{
            "location": "local",
            "local": {
                "port": 6381,
                "version": "6.2",
                "runtime": "auto",
                "options": {
                    "args": ["--maxmemory", "256mb", "--appendonly", "yes"]
                }
            }
        }"#;

        let config = serde_json::from_str::<RedisBranchConfig>(json)
            .unwrap()
            .into_local();

        assert_eq!(config.local.port, 6381);
        assert_eq!(config.local.version, "6.2");
        assert_eq!(config.local.runtime, RedisRuntime::Auto);
        assert_eq!(
            config.local.options.args,
            vec!["--maxmemory", "256mb", "--appendonly", "yes"]
        );
    }

    #[test]
    fn test_direct_values() {
        let json = r#"{
            "location": "local",
            "connection": {
                "host": "localhost",
                "port": 6379,
                "password": "mypassword"
            }
        }"#;

        let config = serde_json::from_str::<RedisBranchConfig>(json)
            .unwrap()
            .into_local();

        assert!(matches!(
            config.connection.host,
            Some(RedisValueSource::Direct(ref h)) if h == "localhost"
        ));
        assert!(matches!(
            config.connection.password,
            Some(RedisValueSource::Direct(ref p)) if p == "mypassword"
        ));
    }

    #[test]
    fn test_default_remote() {
        let json = r#"{
            "connection": {
                "url": {
                    "type": "env",
                    "variable": "REDIS_URL"
                }
            }
        }"#;

        let config = serde_json::from_str::<RedisBranchConfig>(json).unwrap();

        assert!(matches!(config, RedisBranchConfig::Remote(_)));
    }
}
