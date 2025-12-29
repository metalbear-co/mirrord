//! Local Redis management for mirrord.
//!
//! This module handles spawning and managing local Redis instances for the `db_branches`
//! feature when `location: "local"` is configured.

use std::process::{Child, Command, Stdio};

use mirrord_config::{
    container::ContainerRuntime,
    feature::database_branches::{
        RedisConnectionConfig, RedisLocalConfig, RedisRuntime, RedisValueSource,
    },
};
use mirrord_progress::Progress;
use tokio_retry::{Retry, strategy::ExponentialBackoff};

use crate::{CliError, CliResult};

/// Represents a local Redis instance managed by mirrord.
pub enum LocalRedis {
    /// Container (runtime command, container name).
    Container {
        runtime: String,
        container_name: String,
    },
    /// Native redis-server process (child process handle).
    Process(Child),
}

impl Drop for LocalRedis {
    fn drop(&mut self) {
        match self {
            LocalRedis::Container {
                runtime,
                container_name,
            } => {
                let _ = Command::new(runtime)
                    .args(["rm", "-f", container_name])
                    .output();
            }
            LocalRedis::Process(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

/// Build a Redis connection URL from separated connection settings.
///
/// Constructs a URL in the format: `redis://[user:pass@]host[:port][/db]`
#[allow(dead_code)]
pub fn build_connection_url(config: &RedisConnectionConfig) -> Option<String> {
    // If URL is already provided, use it directly
    if let Some(url) = &config.url {
        return match url {
            RedisValueSource::Direct(url) => Some(url.clone()),
            RedisValueSource::Env(_) => None, // Can't resolve env vars here
        };
    }

    // Build from separated settings
    let host = config.host.as_ref().and_then(|h| match h {
        RedisValueSource::Direct(host) => Some(host.clone()),
        RedisValueSource::Env(_) => None,
    })?;

    let port = config.effective_port();
    let db = config.database.unwrap_or(0);

    // Build auth part
    let auth = match (&config.username, &config.password) {
        (Some(RedisValueSource::Direct(user)), Some(RedisValueSource::Direct(pass))) => {
            format!("{}:{}@", user, pass)
        }
        (None, Some(RedisValueSource::Direct(pass))) => {
            format!(":{}@", pass)
        }
        _ => String::new(),
    };

    let scheme = if config.tls == Some(true) {
        "rediss"
    } else {
        "redis"
    };

    Some(format!("{scheme}://{auth}{host}:{port}/{db}"))
}

/// Build the local connection string to inject into the app's environment.
///
/// This determines what format to use based on the original connection config:
/// - If the app was using a URL (e.g., `REDIS_URL=redis://host:6379/0`), we return a URL.
/// - If the app was using host:port (e.g., `REDIS_ADDR=redis-main:6379`), we return host:port.
///
/// The `/0` in URL format is the Redis database number. Database 0 is the default.
pub fn build_local_connection_string(port: u16, config: &RedisConnectionConfig) -> String {
    if config.has_url() {
        // App expects URL format: redis://localhost:6379/0
        // The /0 selects database 0 (default Redis database)
        format!("redis://localhost:{port}/0")
    } else {
        // App expects simple host:port format
        format!("localhost:{port}")
    }
}

/// Start a local Redis instance using the configured runtime.
pub async fn start<P: Progress>(
    progress: &P,
    local_config: &RedisLocalConfig,
) -> CliResult<LocalRedis> {
    match local_config.runtime {
        RedisRuntime::Container => {
            start_container(progress, local_config, local_config.container_runtime).await
        }
        RedisRuntime::RedisServer => start_server(progress, local_config).await,
        RedisRuntime::Auto => {
            // Try configured container runtime first, then fall back to redis-server
            start_container(progress, local_config, local_config.container_runtime)
                .await
                .or_else(|_| futures::executor::block_on(start_server(progress, local_config)))
        }
    }
}

/// Start Redis using a container runtime (Docker, Podman, or nerdctl).
async fn start_container<P: Progress>(
    progress: &P,
    config: &RedisLocalConfig,
    container_runtime: ContainerRuntime,
) -> CliResult<LocalRedis> {
    let runtime_name = container_runtime.command();
    let mut sub = progress.subtask("starting local Redis");
    sub.print(&format!("starting Redis {} container...", runtime_name));

    // Use custom command path if provided, otherwise use runtime name from PATH
    let runtime_cmd = config.container_command.as_deref().unwrap_or(runtime_name);

    let port = config.port;
    let container_name = format!("mirrord-redis-{port}");
    let image = format!("redis:{}", config.version);

    // Remove any existing container with same name
    let _ = Command::new(runtime_cmd)
        .args(["rm", "-f", &container_name])
        .output();

    // Build container run command
    let mut container_args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        container_name.clone(),
        "-p".to_string(),
        format!("{port}:6379"),
        image,
    ];

    // Add any custom Redis args (passed as CMD to the container)
    container_args.extend(config.options.args.iter().cloned());

    let output = Command::new(runtime_cmd)
        .args(&container_args)
        .output()
        .map_err(|e| CliError::LocalRedisError(format!("{runtime_name} failed: {e}")))?;

    if !output.status.success() {
        sub.failure(Some(&format!("failed to start Redis ({runtime_name})")));
        return Err(CliError::LocalRedisError(format!(
            "{runtime_name} run failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    // Wait for Redis to be ready with exponential backoff
    let retry_strategy = ExponentialBackoff::from_millis(100)
        .max_delay(std::time::Duration::from_secs(1))
        .take(10);

    let ready = Retry::spawn(retry_strategy, || async {
        if is_ready(port) { Ok(()) } else { Err(()) }
    })
    .await;

    if ready.is_ok() {
        sub.success(Some(&format!("Redis ({runtime_name}) on localhost:{port}")));
        Ok(LocalRedis::Container {
            runtime: runtime_cmd.to_string(),
            container_name,
        })
    } else {
        sub.failure(Some("Redis container not responding"));
        Err(CliError::LocalRedisError(
            "Redis did not become ready within timeout".into(),
        ))
    }
}

/// Start Redis using native redis-server binary.
async fn start_server<P: Progress>(
    progress: &P,
    config: &RedisLocalConfig,
) -> CliResult<LocalRedis> {
    let mut sub = progress.subtask("starting local Redis");
    sub.print("starting native redis-server...");

    let port = config.port;

    // Use custom server path if provided, otherwise use "redis-server" from PATH
    let server_cmd = config.server_command.as_deref().unwrap_or("redis-server");

    // Build redis-server command with port and any custom args
    let mut redis_args = vec!["--port".to_string(), port.to_string()];
    redis_args.extend(config.options.args.iter().cloned());

    let child = Command::new(server_cmd)
        .args(&redis_args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| CliError::LocalRedisError(format!("{server_cmd} failed: {e}")))?;

    // Wait for Redis to be ready with exponential backoff
    let retry_strategy = ExponentialBackoff::from_millis(100)
        .max_delay(std::time::Duration::from_secs(1))
        .take(10);

    let ready = Retry::spawn(retry_strategy, || async {
        if is_ready(port) { Ok(()) } else { Err(()) }
    })
    .await;

    if ready.is_ok() {
        sub.success(Some(&format!("Redis (native) on localhost:{port}")));
        Ok(LocalRedis::Process(child))
    } else {
        sub.failure(Some("redis-server not responding"));
        Err(CliError::LocalRedisError(
            "Redis did not become ready within timeout".into(),
        ))
    }
}

/// Check if Redis is ready by sending a PING command.
fn is_ready(port: u16) -> bool {
    Command::new("redis-cli")
        .args(["-p", &port.to_string(), "PING"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "PONG")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_connection_url_simple() {
        let config = RedisConnectionConfig {
            host: Some(RedisValueSource::Direct("redis-main".to_string())),
            port: Some(6379),
            ..Default::default()
        };

        assert_eq!(
            build_connection_url(&config),
            Some("redis://redis-main:6379/0".to_string())
        );
    }

    #[test]
    fn test_build_connection_url_with_auth() {
        let config = RedisConnectionConfig {
            host: Some(RedisValueSource::Direct("redis.example.com".to_string())),
            port: Some(6380),
            username: Some(RedisValueSource::Direct("admin".to_string())),
            password: Some(RedisValueSource::Direct("secret".to_string())),
            database: Some(3),
            ..Default::default()
        };

        assert_eq!(
            build_connection_url(&config),
            Some("redis://admin:secret@redis.example.com:6380/3".to_string())
        );
    }

    #[test]
    fn test_build_connection_url_with_tls() {
        let config = RedisConnectionConfig {
            host: Some(RedisValueSource::Direct("secure-redis".to_string())),
            tls: Some(true),
            ..Default::default()
        };

        assert_eq!(
            build_connection_url(&config),
            Some("rediss://secure-redis:6379/0".to_string())
        );
    }

    #[test]
    fn test_build_local_connection_string_url_format() {
        let config = RedisConnectionConfig {
            url: Some(RedisValueSource::Direct(
                "redis://original:6379".to_string(),
            )),
            ..Default::default()
        };

        assert_eq!(
            build_local_connection_string(6379, &config),
            "redis://localhost:6379/0"
        );
    }

    #[test]
    fn test_build_local_connection_string_host_format() {
        let config = RedisConnectionConfig {
            host: Some(RedisValueSource::Direct("redis-main".to_string())),
            ..Default::default()
        };

        assert_eq!(
            build_local_connection_string(6379, &config),
            "localhost:6379"
        );
    }
}
