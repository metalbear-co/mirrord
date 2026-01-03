use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::container::ContainerRuntime;

/// When configuring a branch for Redis, set `type` to `redis`.
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
pub struct RedisBranchConfig {
    /// Optional unique identifier for reusing branches across sessions.
    #[serde(default)]
    pub id: Option<String>,

    /// Where the Redis instance should run.
    /// - `local`: Spawns a local Redis instance.
    /// - `remote`: Uses the remote Redis (default behavior).
    #[serde(default)]
    pub location: RedisBranchLocation,

    /// Connection configuration for the Redis instance.
    #[serde(default)]
    pub connection: RedisConnectionConfig,

    /// Local Redis runtime configuration.
    /// Only used when `location` is `local`.
    #[serde(default)]
    pub local: RedisLocalConfig,
}

/// Location for the Redis branch instance.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RedisBranchLocation {
    /// Use a local Redis instance that mirrord manages.
    Local,
    /// Use the remote Redis (default behavior, no-op).
    #[default]
    Remote,
}

/// Redis connection configuration.
///
/// Supports either a complete URL or separated connection parameters.
/// If both are provided, `url` takes precedence.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize, Default)]
pub struct RedisConnectionConfig {
    /// Complete Redis URL (e.g., `redis://user:pass@host:6379/0`).
    /// Can be sourced from an environment variable.
    #[serde(default)]
    pub url: Option<RedisValueSource>,

    /// Redis host/hostname.
    #[serde(default)]
    pub host: Option<RedisValueSource>,

    /// Redis port (default: 6379).
    #[serde(default)]
    pub port: Option<u16>,

    /// Redis password for authentication.
    #[serde(default)]
    pub password: Option<RedisValueSource>,

    /// Redis username (Redis 6+ ACL).
    #[serde(default)]
    pub username: Option<RedisValueSource>,

    /// Redis database number (default: 0).
    #[serde(default)]
    pub database: Option<u16>,

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
}

/// Source for a Redis configuration value.
///
/// Values can be specified directly or sourced from environment variables.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RedisValueSource {
    /// Direct value.
    Direct(String),
    /// Value sourced from environment.
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

/// Environment variable source for Redis values.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct RedisEnvSource {
    /// Must be "env" to indicate environment variable source.
    #[serde(rename = "type")]
    pub source_type: RedisEnvSourceType,

    /// Name of the environment variable.
    pub variable: String,

    /// Optional container name for multi-container pods.
    #[serde(default)]
    pub container: Option<String>,
}

/// Type marker for environment variable sources.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RedisEnvSourceType {
    Env,
}

/// Configuration for local Redis runtime.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct RedisLocalConfig {
    /// Local port to bind Redis to (default: 6379).
    #[serde(default = "default_local_port")]
    pub port: u16,

    /// Redis version/tag to use (default: "7-alpine").
    /// Used as the container image tag.
    #[serde(default = "default_redis_version")]
    pub version: String,

    /// Runtime backend for local Redis: `container`, `redis_server`, or `auto`.
    #[serde(default)]
    pub runtime: RedisRuntime,

    /// Which container runtime to use (Docker, Podman, or nerdctl).
    /// Only applies when `runtime` is `container` or `auto`.
    #[serde(default)]
    pub container_runtime: ContainerRuntime,

    /// Custom path to the container command.
    /// If not provided, uses the runtime name from PATH (e.g., "docker").
    /// Example: `/usr/local/bin/docker` or `/home/user/.local/bin/podman`
    #[serde(default)]
    pub container_command: Option<String>,

    /// Custom path to the redis-server binary.
    /// If not provided, uses "redis-server" from PATH.
    /// Example: `/opt/redis/bin/redis-server`
    #[serde(default)]
    pub server_command: Option<String>,

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

/// Runtime backend for running local Redis.
///
/// For container-based runtimes, mirrord spawns the Redis image in a container.
/// For `redis_server`, it runs the native binary directly.
///
/// Backends:
/// - `container` (default) - Uses a container runtime (Docker/Podman/nerdctl)
/// - `redis_server` - Uses native redis-server binary
/// - `auto` - Tries container first, falls back to redis-server
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RedisRuntime {
    /// Use a container runtime (configure which one via `container_runtime`). (default)
    #[default]
    Container,
    /// Use native redis-server binary.
    RedisServer,
    /// Auto-detect: try container first, fall back to redis-server.
    Auto,
}

/// Additional arguments passed to the Redis server.
///
/// Example:
/// ```json
/// {
///   "args": ["--maxmemory", "256mb", "--appendonly", "yes"]
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize, Default)]
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

        let config: RedisBranchConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.location, RedisBranchLocation::Local);
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

        let config: RedisBranchConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.location, RedisBranchLocation::Local);
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

        let config: RedisBranchConfig = serde_json::from_str(json).unwrap();
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

        let config: RedisBranchConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(
            config.connection.host,
            Some(RedisValueSource::Direct(ref h)) if h == "localhost"
        ));
        assert!(matches!(
            config.connection.password,
            Some(RedisValueSource::Direct(ref p)) if p == "mypassword"
        ));
    }
}
