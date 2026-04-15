use std::{
    path::PathBuf,
    process::{ExitStatus, Stdio},
};

use miette::Diagnostic;
use mirrord_config::config::{ConfigError, EnvKey};
use thiserror::Error;
mod config;

pub use config::{SubprocessCfg, UpConfig};
use mirrord_progress::MIRRORD_PROGRESS_ENV;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    task::{JoinError, JoinSet},
};

pub const RESOLVED_CONFIG_ENV: &str = "MIRRORD_UP_RESOLVED_CONFIG";

/// Errors produced by `mirrord up` command.
#[derive(Debug, Error, Diagnostic)]
pub enum UpError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("failed to parse mirrord-up config: {0}")]
    #[diagnostic(help("Check the YAML syntax and field names in your mirrord-up.yaml."))]
    Parse(#[from] serde_yaml::Error),

    #[error("mirrord-up config validation failed: {0}")]
    Validation(#[from] ConfigError),

    #[error("Service {name} crashed with exit status {status}")]
    ServiceCrashed { name: String, status: ExitStatus },

    #[error("Child process handler task panicked: {0:?}")]
    Panic(JoinError),
}

/// Load and parse a `mirrord-up.yaml` configuration file.
pub fn load_up_config(path: &PathBuf) -> Result<UpConfig, UpError> {
    let content = std::fs::read_to_string(path)?;
    Ok(serde_yaml::from_str(&content)?)
}

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
