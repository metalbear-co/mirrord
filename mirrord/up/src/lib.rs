//! Implementation of the `mirrord up` command, which runs multiple mirrord sessions
//! from a single configuration file.

#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]
#![deny(missing_docs)]

use std::{
    path::PathBuf,
    process::{ExitStatus, Stdio},
};

use miette::Diagnostic;
use mirrord_config::config::{ConfigError, EnvKey};
use thiserror::Error;
mod config;

pub use config::{ServiceMode, SubprocessCfg, UpConfig};
use mirrord_progress::MIRRORD_PROGRESS_ENV;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    task::{JoinError, JoinSet},
};

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
/// Returns when one of the child mirrord sessions exits.
pub async fn run(up_config: UpConfig, key: EnvKey) -> Result<(), UpError> {
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

    let mut handles = JoinSet::new();
    for (name, mut command) in commands {
        let mut child = command.spawn().unwrap();
        handles.spawn(async move {
            let mut err = BufReader::new(child.stderr.take().unwrap()).lines();
            let mut out = BufReader::new(child.stdout.take().unwrap()).lines();

            loop {
                tokio::select! {
                    line = out.next_line() => match line {
                        Ok(Some(line)) => println!("{name}: {line}"),
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
