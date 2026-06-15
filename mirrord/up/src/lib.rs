//! Implementation of the `mirrord up` command, which runs multiple mirrord sessions
//! from a single configuration file.

#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]
#![deny(missing_docs)]

use std::{
    ops::Not,
    path::PathBuf,
    process::{ExitStatus, Stdio},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use miette::Diagnostic;
use mirrord_analytics::MIRRORD_UP_CORRELATION_ID_ENV;
use mirrord_config::config::{ConfigError, EnvKey};
use thiserror::Error;
mod config;
mod init;

pub use config::{ServiceMode, SubprocessCfg, UpConfig};
pub use init::{InitError, run_wizard};
use mirrord_progress::{MIRRORD_PROGRESS_ENV, messages::SESSION_READY_MESSAGE};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    task::{JoinError, JoinSet},
};
use uuid::Uuid;

/// Shared slot that [`run`] fills with the time it took for **all** child
/// sessions to become ready, measured from process spawn.
///
/// The caller constructs one, passes a clone to [`run`], and reads
/// [`ReadyTracker::time_to_ready`] afterwards (the value is captured even if
/// `run` later returns an error, as long as readiness was reached first).
#[derive(Clone, Default)]
pub struct ReadyTracker {
    elapsed: Arc<OnceLock<Duration>>,
}

impl ReadyTracker {
    /// The time from spawn until every session became ready, if that happened.
    pub fn time_to_ready(&self) -> Option<Duration> {
        self.elapsed.get().copied()
    }
}

/// Environment variable used to pass the resolved configuration to child mirrord processes.
pub const RESOLVED_CONFIG_ENV: &str = "MIRRORD_UP_RESOLVED_CONFIG";

/// Errors produced by `mirrord up` command.
#[derive(Debug, Error, Diagnostic)]
pub enum UpError {
    /// IO error.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Failed to parse the mirrord-up YAML configuration.
    #[error("failed to parse mirrord-up config: {0}")]
    #[diagnostic(help("Check the YAML syntax and field names in your mirrord-up.yaml."))]
    Parse(#[from] serde_yaml::Error),

    /// Configuration validation failed.
    #[error("mirrord-up config validation failed: {0}")]
    Validation(#[from] ConfigError),

    /// A child mirrord service exited with a non-zero status.
    #[error("Service {name} crashed with exit status {status}")]
    ServiceCrashed {
        /// Name of the service that crashed.
        name: String,
        /// Exit status of the crashed service.
        status: ExitStatus,
    },

    /// A child process handler task panicked.
    #[error("Child process handler task panicked: {0:?}")]
    Panic(JoinError),
}

/// Load and parse a `mirrord-up.yaml` configuration file.
pub fn load_up_config(path: &PathBuf) -> Result<UpConfig, UpError> {
    let content = std::fs::read_to_string(path)?;
    Ok(serde_yaml::from_str(&content)?)
}

/// Genererate [`mirrord_config::LayerConfig`]s based on the provided [`UpConfig`] and
/// [`EnvKey`] and spawn child mirrord processes. Stdout/stderr from
/// children will be printed to the console, prefixed with the name of
/// the session.
///
/// `ready` is filled with the time-to-ready once every session has signalled
/// readiness (see [`ReadyTracker`]).
///
/// Returns when one of the child mirrord sessions exits.
pub async fn run(
    up_config: UpConfig,
    key: EnvKey,
    correlation_id: Uuid,
    ready: ReadyTracker,
) -> Result<(), UpError> {
    let commands: Vec<_> = up_config
        .service_configs(&key)
        .map(|config| {
            let SubprocessCfg {
                config,
                service_name,
                run,
            } = config;

            let encoded_cfg = config.encode()?;

            let mut cmd = Command::new(std::env::current_exe()?);
            cmd.env(RESOLVED_CONFIG_ENV, encoded_cfg)
                .env(MIRRORD_PROGRESS_ENV, "simple")
                .env(MIRRORD_UP_CORRELATION_ID_ENV, correlation_id.to_string())
                .arg(Into::<&'static str>::into(run.r#type))
                .arg("--")
                .args(run.command)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);

            Ok((service_name, cmd))
        })
        .collect::<Result<_, UpError>>()?;

    let start = Instant::now();
    let total = commands.len();
    let ready_count = Arc::new(AtomicUsize::new(0));

    let mut handles = JoinSet::new();
    for (name, mut command) in commands {
        let mut child = command.spawn().unwrap();
        let ready_count = Arc::clone(&ready_count);
        let ready = ready.clone();
        handles.spawn(async move {
            let mut err = BufReader::new(child.stderr.take().unwrap()).lines();
            let mut out = BufReader::new(child.stdout.take().unwrap()).lines();

            // A session is only counted once: the user's binary inherits the
            // same stdout after `execve`, so a later line matching the marker
            // must not be double-counted.
            let mut counted = false;

            loop {
                tokio::select! {
                    line = out.next_line() => match line {
                        Ok(Some(line)) => {
                            if counted.not() && line.trim() == SESSION_READY_MESSAGE {
                                counted = true;
                                if ready_count.fetch_add(1, Ordering::Relaxed) + 1 == total {
                                    // TODO(areg) downgrade to a
                                    // `debug_assert` once the feature
                                    // stabilizes.
                                    ready.elapsed.set(start.elapsed()).expect("only the final task should set the ready marker");
                                }
                            }
                            println!("{name}: {line}");
                        }
                        Ok(None) => {}
                        Err(err) => println!("{name} error: {err:?}"),
                    },

                    line = err.next_line() => match line {
                        Ok(Some(line)) => println!("{name}: {line}"),
                        Ok(None) => {}
                        Err(err) => println!("{name} error: {err:?}"),
                    },

                    status = child.wait() => {
                        let status = status?;
                        if status.success() {
                            break Ok(());
                        } else {
                            break Err(UpError::ServiceCrashed {
                                name,
                                status,
                            })
                        }
                    }
                }
            }
        });
    }

    let Some(status) = handles.join_next().await else {
        unreachable!("should have at least one service")
    };

    // Handle JoinError and UpError from child handler tasks
    status.map_err(UpError::Panic)??;

    Ok(())
}
