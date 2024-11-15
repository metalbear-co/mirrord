use std::{path::PathBuf, time::Duration};

use mirrord_analytics::NullReporter;
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    LayerConfig, LayerFileConfig,
};
use mirrord_operator::{
    client::{NoClientCert, OperatorApi},
    crd::MirrordOperatorSpec,
    types::LicenseInfoOwned,
};
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{row, Table};
use tracing::Level;

use crate::{util::remove_proxy_env, CliError, Result};

/// Handles the `mirrord operator status` command.
pub(super) struct StatusCommandHandler {
    /// Api to talk with session routes in the operator.
    operator_api: OperatorApi<NoClientCert>,
}

impl StatusCommandHandler {
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn new(config_file: Option<PathBuf>) -> Result<Self> {
        let mut progress = ProgressTracker::from_env("Operator Status");

        let layer_config = if let Some(config) = config_file {
            let mut cfg_context = ConfigContext::default();
            LayerFileConfig::from_path(config)?.generate_config(&mut cfg_context)?
        } else {
            LayerConfig::from_env()?
        };

        if !layer_config.use_proxy {
            remove_proxy_env();
        }

        let mut status_progress = progress.subtask("fetching status");
        let api = OperatorApi::try_new(&layer_config, &mut NullReporter::default())
            .await
            .inspect_err(|_| {
                status_progress.failure(Some("failed to get status"));
            })?
            .ok_or(CliError::OperatorNotInstalled)?;

        status_progress.success(Some("fetched status"));
        progress.success(None);

        Ok(Self { operator_api: api })
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err)]
    pub(super) async fn handle(self) -> Result<()> {
        let Self { operator_api: api } = self;

        let MirrordOperatorSpec {
            operator_version,
            default_namespace,
            license:
                LicenseInfoOwned {
                    name,
                    organization,
                    expire_at,
                    ..
                },
            ..
        } = &api.operator().spec;

        let expire_at = expire_at.format("%e-%b-%Y");

        println!(
            r#"
Operator version: {operator_version}
Operator default namespace: {default_namespace}
Operator License
    name: {name}
    organization: {organization}
    expire at: {expire_at}
"#
        );

        // TODO(alex) [low] 1: Looks like a backward compat special case, the early return
        // not being an error here.
        let status = api
            .operator()
            .status
            .as_ref()
            .ok_or(CliError::OperatorStatusNotFound)?;

        if let Some(copy_targets) = status.copy_targets.as_ref() {
            if copy_targets.is_empty() {
                println!("No active copy targets.");
            } else {
                println!("Active Copy Targets:");
                let mut copy_targets_table = Table::new();

                copy_targets_table.add_row(row![
                    "Original Target",
                    "Namespace",
                    "Copy Pod Name",
                    "Scale Down?"
                ]);

                for (pod_name, copy_target_resource) in copy_targets {
                    copy_targets_table.add_row(row![
                        copy_target_resource.spec.target.to_string(),
                        copy_target_resource
                            .metadata
                            .namespace
                            .as_deref()
                            .unwrap_or_default(),
                        pod_name,
                        if copy_target_resource.spec.scale_down {
                            "*"
                        } else {
                            ""
                        },
                    ]);
                }

                copy_targets_table.printstd();
            }
            println!();
        }

        if let Some(statistics) = status.statistics.as_ref() {
            println!("Operator Daily Users: {}", statistics.dau);
            println!("Operator Monthly Users: {}", statistics.mau);
        }

        let mut sessions = Table::new();

        sessions.add_row(row![
            "Session ID",
            "Target",
            "Namespace",
            "User",
            "Ports",
            "Session Duration"
        ]);

        let mut sqs = Table::new();
        sqs.add_row(row!["Session ID", "SQS"]);

        for session in &status.sessions {
            let locked_ports = session
                .locked_ports
                .as_deref()
                .map(|ports| {
                    ports
                        .iter()
                        .map(|(port, type_, filter)| {
                            format!(
                                "Port: {port}, Type: {type_}{}",
                                filter
                                    .as_ref()
                                    .map(|f| format!(", Filter: {}", f))
                                    .unwrap_or_default()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();

            sessions.add_row(row![
                session.id.as_deref().unwrap_or_default(),
                &session.target,
                session.namespace.as_deref().unwrap_or("N/A"),
                &session.user,
                locked_ports,
                humantime::format_duration(Duration::from_secs(session.duration_secs)),
            ]);

            if let Some(session_sqs) = session.sqs.as_deref() {
                sqs.add_row(row![session.id.as_deref().unwrap_or_default(), session_sqs]);
            }
        }

        sessions.printstd();

        if sqs.get_row(1).is_some() {
            println!("Active SQS:");
            sqs.printstd();
        }

        Ok(())
    }
}
