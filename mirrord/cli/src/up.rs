//! The `mirrord up` command - runs multiple mirrord sessions from a `mirrord-up.yaml` file.

use std::{io::ErrorKind, process::Stdio};

use miette::Diagnostic;
use mirrord_analytics::{Analytics, AnalyticsReporter, CollectAnalytics, Reporter};
use mirrord_config::config::EnvKey;
use mirrord_operator::crd::session::UpSessionInfo;
use mirrord_up::{InitError, ReadyTracker, UpError, load_up_config, run_wizard};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    config::{UpArgs, UpSubcommand},
    user_data::UserData,
};

/// Context for sessions started by `mirrord up`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MirrordUp {
    pub(crate) auto_queue_splitting: bool,
}

impl MirrordUp {
    pub(crate) fn from_env() -> Option<Self> {
        std::env::var_os(mirrord_up::RESOLVED_CONFIG_ENV)
            .is_some()
            .then_some(Self {
                auto_queue_splitting: true,
            })
    }

    pub(crate) fn info(&self) -> UpSessionInfo {
        UpSessionInfo {
            auto_queue_splitting: self.auto_queue_splitting.then_some(true),
        }
    }
}

#[derive(Debug, Error, Diagnostic)]
pub enum UpCliError {
    #[error(transparent)]
    Up(#[from] UpError),

    #[error(transparent)]
    Init(#[from] InitError),

    #[error("mirrord-up.yaml file not found.")]
    #[diagnostic(help(
        "Running `mirrord up` requires a `mirrord-up.yaml` configuration file.
- You can create one with `mirrord up init`, or;
- If the file is in another directory, run it with `mirrord up -f <file-path.yaml>`.
        "
    ))]
    ConfigNotFound,

    #[error("failed to acquire OS username for key")]
    #[diagnostic(help(
        "The username is used for automatically generating a session key. Please provide a session key manually or ensure your OS user has a valid username."
    ))]
    UsernameFetch(whoami::Error),
}

/// The `mirrord up` command handler.
pub(crate) async fn up_command(
    args: UpArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> Result<(), UpCliError> {
    if let Some(UpSubcommand::Init { output }) = args.command {
        return Ok(run_wizard(output)?);
    }

    // We don't yet know whether the user has opted out of telemetry,
    // that's stored in the YAML we haven't loaded. Default to enabled
    // so a config-load failure still surfaces as a
    // `config_validation` event; downgrade once the config is read.
    let mut analytics = AnalyticsReporter::for_up_event(true, watch, user_data.machine_id());

    let result = run_up(args, &mut analytics).await;

    record_outcome(&result, analytics.get_mut());
    result
}

async fn run_up(args: UpArgs, analytics: &mut AnalyticsReporter) -> Result<(), UpCliError> {
    let up_config = match load_up_config(&args.config_file) {
        Ok(cfg) => cfg,
        Err(UpError::Io(err)) if err.kind() == ErrorKind::NotFound => {
            return Err(UpCliError::ConfigNotFound);
        }
        other_error => other_error?,
    };

    analytics.enabled = up_config.telemetry_enabled();

    (&up_config).collect_analytics(analytics.get_mut());

    let key = match args.key {
        Some(key) => EnvKey::Provided(key),
        None => EnvKey::Generated(whoami::username().map_err(UpCliError::UsernameFetch)?),
    };
    analytics.get_mut().add("has_custom_key", key.is_provided());

    // Generated once per invocation and propagated to every child session so
    // their `client_session_v1` events can be joined back to this event.
    let correlation_id = Uuid::new_v4();
    analytics.get_mut().add("correlation_id", correlation_id);

    let ready = ReadyTracker::default();

    // Run UI
    analytics.get_mut().add("ui_enabled", args.ui);

    if let Ok(mirrord_binary) = std::env::current_exe()
        && args.ui
    {
        match tokio::process::Command::new(mirrord_binary)
            .args(vec!["ui", "--no-browser"])
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(false)
            .spawn()
        {
            Ok(child) => {
                tracing::info!(
                    child_pid = ?child.id(),
                    "spawned `mirrord ui` command",
                );
            }
            Err(err) => {
                tracing::warn!(?err, "failed to start local UI");
            }
        }
    }

    let result = mirrord_up::run(up_config, key, correlation_id, ready.clone()).await;

    // Recorded whenever all sessions reached readiness, even if `run` then
    // failed, left absent otherwise (e.g. a session crashed before startup).
    if let Some(elapsed) = ready.time_to_ready() {
        analytics
            .get_mut()
            .add("time_to_ready_seconds", elapsed.as_secs());
    }

    Ok(result?)
}

/// Coarse bucket assigned to a failed `mirrord up` invocation for analytics.
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
enum ErrorCategory {
    ConfigValidation = 1,
    ServiceCrash = 2,
    InternalError = 3,
}

impl From<&UpCliError> for ErrorCategory {
    fn from(err: &UpCliError) -> Self {
        match err {
            UpCliError::ConfigNotFound
            | UpCliError::UsernameFetch(_)
            | UpCliError::Up(UpError::Parse(_))
            | UpCliError::Up(UpError::Validation(_)) => Self::ConfigValidation,
            UpCliError::Up(UpError::ServiceCrashed { .. }) => Self::ServiceCrash,
            UpCliError::Up(UpError::Io(_))
            | UpCliError::Up(UpError::Panic(_))
            | UpCliError::Init(_) => Self::InternalError,
        }
    }
}

fn record_outcome(result: &Result<(), UpCliError>, analytics: &mut Analytics) {
    analytics.add("success", result.is_ok());
    if let Err(err) = result {
        analytics.add("error_category", ErrorCategory::from(err) as u32);
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use std::process::ExitStatus;

    use super::*;

    fn category(err: UpCliError) -> serde_json::Value {
        let mut analytics = Analytics::default();
        record_outcome(&Err(err), &mut analytics);
        serde_json::to_value(&analytics).unwrap()
    }

    #[test]
    fn success_records_no_category() {
        let mut analytics = Analytics::default();
        record_outcome(&Ok(()), &mut analytics);
        let v = serde_json::to_value(&analytics).unwrap();
        assert_eq!(v["success"], true);
        assert!(v.get("error_category").is_none());
    }

    #[test]
    fn config_not_found_buckets_as_config_validation() {
        let v = category(UpCliError::ConfigNotFound);
        assert_eq!(v["success"], false);
        assert_eq!(v["error_category"], ErrorCategory::ConfigValidation as u32);
    }

    #[test]
    fn parse_error_buckets_as_config_validation() {
        let parse_err: serde_yaml::Error = serde_yaml::from_str::<i32>("not a number").unwrap_err();
        let v = category(UpCliError::Up(UpError::Parse(parse_err)));
        assert_eq!(v["error_category"], ErrorCategory::ConfigValidation as u32);
    }

    #[test]
    fn service_crash_buckets_as_service_crash() {
        let v = category(UpCliError::Up(UpError::ServiceCrashed {
            name: "svc".to_owned(),
            status: ExitStatus::default(),
        }));
        assert_eq!(v["error_category"], ErrorCategory::ServiceCrash as u32);
    }

    #[test]
    fn io_error_buckets_as_internal_error() {
        let io = std::io::Error::other("boom");
        let v = category(UpCliError::Up(UpError::Io(io)));
        assert_eq!(v["error_category"], ErrorCategory::InternalError as u32);
    }
}
