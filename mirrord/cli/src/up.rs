//! The `mirrord up` command - runs multiple mirrord sessions from a `mirrord-up.yaml` file.

use std::{collections::HashMap, io::ErrorKind, ops::Not, sync::Arc};

use miette::Diagnostic;
use mirrord_analytics::{
    AnalyticValue, Analytics, AnalyticsReporter, CollectAnalytics, Reporter, SanitizedString,
    UpSessionDbBranch,
};
use mirrord_config::config::EnvKey;
use mirrord_operator::{client::UpSession, crd::session::UpSessionInfo};
use mirrord_up::{
    InitError, ReadyTracker, SERVICE_NAME_ENV, ServiceMode, UpError, load_up_config, run_wizard,
};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    config::{UI_DEFAULT_PORT, UiCommonArgs, UpArgs, UpSubcommand},
    data::{GlobalConfig, UserData},
    error::CliError,
    ui::ui_command,
};

pub(crate) mod db_branches;

/// Context for sessions started by `mirrord up`.
#[derive(Clone, Debug)]
pub(crate) struct MirrordUp {
    pub(crate) auto_queue_splitting: bool,
    /// The service the session runs.
    pub(crate) service: Arc<str>,
}

impl MirrordUp {
    /// `mirrord up` sets both its resolved config and the service name on every session it
    /// starts, so a session missing either wasn't started by it.
    pub(crate) fn from_env() -> Option<Self> {
        std::env::var_os(mirrord_up::RESOLVED_CONFIG_ENV)?;
        let service = std::env::var(SERVICE_NAME_ENV).ok()?;
        Some(Self {
            auto_queue_splitting: true,
            service: service.into(),
        })
    }

    pub(crate) fn session(&self) -> UpSession {
        UpSession {
            info: UpSessionInfo {
                auto_queue_splitting: self.auto_queue_splitting.then_some(true),
            },
            service: self.service.clone(),
        }
    }
}

#[derive(Debug, Error, Diagnostic)]
pub enum UpCliError {
    #[error(transparent)]
    #[diagnostic(transparent)]
    Up(#[from] UpError),

    #[error(transparent)]
    Init(#[from] Box<InitError>),

    #[error("mirrord-up.yaml file not found.")]
    #[diagnostic(help(
        "Running `mirrord up` requires a `mirrord-up.yaml` configuration file.
- You can create one with `mirrord up init`, or;
- If the file is in another directory, run it with `mirrord up -f <file-path.yaml>`.
        "
    ))]
    ConfigNotFound,

    /// The operator couldn't tell what a service's DB branching request resolves to, or
    /// rejected it. Shown as the session would show it (including its help, e.g. to upgrade the
    /// operator), prefixed with the service.
    #[error("{service}: {error}")]
    #[diagnostic(forward(error))]
    DbBranches {
        service: Arc<str>,
        error: Box<CliError>,
    },

    #[error("failed to acquire OS username for key")]
    #[diagnostic(help(
        "The username is used for automatically generating a session key. Please provide a session key manually or ensure your OS user has a valid username."
    ))]
    UsernameFetch(whoami::Error),
}

/// The `mirrord up` command handler.
#[mirrord_macros::boxed_future]
pub(crate) async fn up_command(
    args: UpArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> Result<(), UpCliError> {
    if let Some(UpSubcommand::Init { output }) = args.command {
        return Ok(run_wizard(output).await.map_err(Box::new)?);
    }

    // We don't yet know whether the user has opted out of telemetry,
    // that's stored in the YAML we haven't loaded. Default to enabled
    // so a config-load failure still surfaces as a
    // `config_validation` event; downgrade once the config is read.
    let mut analytics = AnalyticsReporter::for_up_event(true, watch, user_data.machine_id());

    let result = normalize_cancellation(run_up(args, &mut analytics).await);

    record_outcome(&result, analytics.get_mut());
    result
}

async fn run_up(args: UpArgs, analytics: &mut AnalyticsReporter) -> Result<(), UpCliError> {
    let key = match args.key {
        Some(key) => EnvKey::Provided(key),
        None => EnvKey::Generated(whoami::username().map_err(UpCliError::UsernameFetch)?),
    };

    let mut up_config = match load_up_config(&args.config_file, &key) {
        Ok(cfg) => cfg,
        Err(UpError::Io(err)) if err.kind() == ErrorKind::NotFound => {
            return Err(UpCliError::ConfigNotFound);
        }
        other_error => other_error?,
    };

    up_config.default_context(GlobalConfig::load().await.kube_context().map(Arc::from));

    analytics.enabled = up_config.telemetry_enabled();

    (&up_config).collect_analytics(analytics.get_mut());
    analytics
        .get_mut()
        .add("num_selected_services", args.services.len());

    up_config
        .select_services(&args.services)
        .map_err(UpError::from)?;

    analytics
        .get_mut()
        .add("mode_override", args.mode.is_some());
    if let Some(mode) = &args.mode {
        up_config.override_mode(mode).map_err(UpError::from)?;
    }

    #[cfg(target_os = "windows")]
    up_config
        .validate_windows(std::env::var_os(crate::ci::MIRRORD_CI_API_KEY).is_some())
        .map_err(UpError::from)?;

    analytics.get_mut().add("has_custom_key", key.is_provided());

    // Generated once per invocation and propagated to every child session so
    // their `client_session_v1` events can be joined back to this event.
    let correlation_id = Uuid::new_v4();
    analytics.get_mut().add("correlation_id", correlation_id);

    let ready = ReadyTracker::default();

    analytics.get_mut().add("ui_enabled", !args.no_ui);
    if !args.no_ui {
        tokio::spawn(async move {
            match ui_command(
                UiCommonArgs {
                    config_file: None,
                    port: UI_DEFAULT_PORT,
                    no_browser: true,
                },
                None,
                "/",
            )
            .await
            {
                Ok(_) => {
                    tracing::info!("spawned `mirrord ui` command");
                }
                Err(err) => {
                    tracing::warn!(?err, "failed to start local UI");
                }
            }
        });
    }

    let mut services =
        mirrord_up::resolve_services(up_config, &args.config_file, args.context, &key).await?;
    let requesting = db_branches::check_db_branches(&mut services).await?;
    let modes = services
        .iter()
        .map(|service| (service.service_name.clone(), service.mode.clone()))
        .collect::<Vec<_>>();

    let result = mirrord_up::launch(services, correlation_id, ready.clone()).await;

    analytics.get_mut().add(
        "services",
        service_analytics(&modes, &requesting, &ready.db_branches()),
    );

    // Recorded whenever all sessions reached readiness, even if `run` then
    // failed, left absent otherwise (e.g. a session crashed before startup).
    if let Some(elapsed) = ready.time_to_ready() {
        analytics
            .get_mut()
            .add("time_to_ready_seconds", elapsed.as_secs());
    }

    Ok(result?)
}

/// One entry per service, in no particular order and without names: its mode (`custom` for one
/// defined under `modes`, whose name is the user's), whether it requested
/// DB branching, and whether its session created a branch, attached to an existing one, and shares
/// one with another service of the run.
fn service_analytics(
    modes: &[(Arc<str>, Arc<str>)],
    requesting: &[Arc<str>],
    db_branches: &HashMap<Arc<str>, Vec<UpSessionDbBranch>>,
) -> Vec<AnalyticValue> {
    // Branch names are unique within a cluster only.
    let mut users: HashMap<(&str, &str), usize> = HashMap::new();
    for branch in db_branches.values().flatten() {
        *users
            .entry((branch.cluster.as_str(), branch.name.as_str()))
            .or_default() += 1;
    }

    modes
        .iter()
        .map(|(name, mode)| {
            let branches = db_branches.get(name).map(Vec::as_slice).unwrap_or_default();
            let mut service = Analytics::default();
            let mode = ServiceMode::from_name(mode).map_or("custom", <&str>::from);
            service.add("mode", SanitizedString::new_plaintext(mode));
            service.add("db_branching_requested", requesting.contains(name));
            service.add(
                "db_branch_created",
                branches.iter().any(|branch| branch.created),
            );
            service.add(
                "db_branch_attached",
                branches.iter().any(|branch| branch.created.not()),
            );
            service.add(
                "db_branch_shared",
                branches.iter().any(|branch| {
                    users
                        .get(&(branch.cluster.as_str(), branch.name.as_str()))
                        .copied()
                        .unwrap_or_default()
                        > 1
                }),
            );
            service.into()
        })
        .collect()
}

/// Coarse bucket assigned to a failed `mirrord up` invocation for analytics.
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
enum ErrorCategory {
    ConfigValidation = 1,
    ServiceCrash = 2,
    InternalError = 3,
    PreCommandFailed = 4,
    ServiceStartFailed = 5,
    DbBranching = 6,
}

impl From<&UpCliError> for ErrorCategory {
    fn from(err: &UpCliError) -> Self {
        match err {
            UpCliError::ConfigNotFound
            | UpCliError::UsernameFetch(_)
            | UpCliError::Up(UpError::Parse(_))
            | UpCliError::Up(UpError::InvalidRunDirectory { .. })
            | UpCliError::Up(UpError::ContainerRunDirectory { .. })
            | UpCliError::Up(UpError::Select(_))
            | UpCliError::Up(UpError::Validation(_))
            | UpCliError::Up(UpError::ConfigPatch(_))
            | UpCliError::Up(UpError::Tera(_))
            | UpCliError::Up(UpError::Mode(_))
            | UpCliError::Up(UpError::BuiltinModeName(_))
            | UpCliError::Up(UpError::UnknownMode(_))
            | UpCliError::Up(UpError::NoInferredTarget { .. })
            | UpCliError::Up(UpError::PreviewTargetless { .. })
            | UpCliError::Up(UpError::WindowsUnsupported(_)) => Self::ConfigValidation,
            UpCliError::Up(UpError::ServiceCrashed { .. }) => Self::ServiceCrash,
            UpCliError::Up(UpError::ServiceFailedToStart { .. }) => Self::ServiceStartFailed,
            UpCliError::DbBranches { .. } => Self::DbBranching,
            UpCliError::Up(UpError::PreCommandFailed { .. })
            | UpCliError::Up(UpError::PreCommandSpawn { .. }) => Self::PreCommandFailed,
            UpCliError::Up(UpError::Io(_))
            | UpCliError::Up(UpError::Panic(_))
            | UpCliError::Up(UpError::Kube(_))
            | UpCliError::Up(UpError::Inquire(_))
            | UpCliError::Up(UpError::Exited)
            | UpCliError::Up(UpError::NoWorkloads(_))
            | UpCliError::Up(UpError::YamlPatch(_))
            | UpCliError::Up(UpError::YamlQuery(_))
            | UpCliError::Init(_) => Self::InternalError,
        }
    }
}

fn normalize_cancellation(result: Result<(), UpCliError>) -> Result<(), UpCliError> {
    match result {
        Err(UpCliError::Up(error)) if error.is_user_cancelled() => Ok(()),
        result => result,
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

    use mirrord_config::target::TargetType;
    use mirrord_up::{IncompatibleTarget, SelectError, ServiceMode, TargetValidationError};

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
        let value = serde_json::to_value(&analytics).unwrap();
        assert_eq!(value["success"], true);
        assert!(value.get("error_category").is_none());
    }

    #[test]
    fn wizard_exit_records_success_without_error_category() {
        let result = normalize_cancellation(Err(UpCliError::Up(UpError::Exited)));
        assert!(result.is_ok());
        let mut analytics = Analytics::default();
        record_outcome(&result, &mut analytics);
        let value = serde_json::to_value(&analytics).unwrap();
        assert_eq!(value["success"], true);
        assert!(value.get("error_category").is_none());
    }

    #[test]
    fn cancellation_normalization_preserves_failures() {
        assert!(matches!(
            normalize_cancellation(Err(UpCliError::ConfigNotFound)),
            Err(UpCliError::ConfigNotFound)
        ));
    }

    #[test]
    fn config_not_found_buckets_as_config_validation() {
        let v = category(UpCliError::ConfigNotFound);
        assert_eq!(v["success"], false);
        assert_eq!(v["error_category"], ErrorCategory::ConfigValidation as u32);
    }

    #[test]
    fn up_errors_keep_their_help() {
        let error = UpCliError::Up(UpError::Select(SelectError::UnknownServices {
            unknown: vec!["nope".to_owned()],
            available: vec!["api".to_owned()],
        }));

        let help = error.help().map(|help| help.to_string());
        assert_eq!(
            help.as_deref(),
            Some("Available services: api"),
            "`UpCliError::Up` must forward the wrapped error's help"
        );
    }

    #[test]
    fn parse_error_buckets_as_config_validation() {
        let parse_err: serde_saphyr::Error =
            serde_saphyr::from_str::<i32>("not a number").unwrap_err();
        let v = category(UpCliError::Up(UpError::Parse(parse_err)));
        assert_eq!(v["error_category"], ErrorCategory::ConfigValidation as u32);
    }

    #[test]
    fn mode_error_buckets_as_config_validation() {
        let v = category(UpCliError::Up(UpError::Mode(vec![
            TargetValidationError::IncompatibleTargets(vec![IncompatibleTarget {
                service: "svc".into(),
                mode: ServiceMode::Replace.into(),
                incoming: ServiceMode::Replace,
                target_type: TargetType::Targetless,
            }]),
        ])));
        assert_eq!(v["error_category"], ErrorCategory::ConfigValidation as u32);
    }

    #[test]
    fn service_crash_buckets_as_service_crash() {
        let v = category(UpCliError::Up(UpError::ServiceCrashed {
            name: "svc".into(),
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
