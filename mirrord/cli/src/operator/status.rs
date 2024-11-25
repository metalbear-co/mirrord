use std::{path::PathBuf, time::Duration};

use kube::{api::ListParams, Api};
use mirrord_analytics::NullReporter;
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    LayerConfig, LayerFileConfig,
};
use mirrord_operator::{
    client::{NoClientCert, OperatorApi},
    crd::{is_session_ready, MirrordOperatorSpec, MirrordSqsSession, QueueNameUpdate},
    types::LicenseInfoOwned,
};
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{row, Table};
use tracing::Level;

use crate::{error::CliError, util::remove_proxy_env, CliResult};

/// Handles the `mirrord operator status` command.
pub(super) struct StatusCommandHandler {
    /// Api to talk with session routes in the operator.
    operator_api: OperatorApi<NoClientCert>,
}

impl StatusCommandHandler {
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn new(config_file: Option<PathBuf>) -> CliResult<Self> {
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
            .ok_or(CliError::OperatorNotInstalled)
            .inspect_err(|_| status_progress.failure(Some("operator not found")))?;

        status_progress.success(Some("fetched status"));
        progress.success(None);

        Ok(Self { operator_api: api })
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err)]
    pub(super) async fn handle(self) -> CliResult<()> {
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
        }

        sessions.printstd();
        println!();

        let sqs_sessions_api = Api::<MirrordSqsSession>::all(api.client().clone());
        for (sqs_session_spec, sqs_session_table) in sqs_sessions_api
            .list(&ListParams::default())
            .await
            .map_err(CliError::ListSqsSessions)?
            .into_iter()
            .filter(|session| is_session_ready(Some(session)))
            .filter_map(|session| {
                Some((
                    session
                        .status
                        .as_ref()?
                        .get_split_details()?
                        .clone()
                        .queue_names
                        .into_iter(),
                    session,
                ))
            })
            .map(|(split_details, session)| {
                let mut session_table = Table::new();
                session_table.add_row(row!["Queue Id", "Original Name", "Output Name", "Filter"]);

                let filters = &session.spec.queue_filters;

                for (
                    id,
                    QueueNameUpdate {
                        output_name,
                        original_name,
                    },
                ) in split_details
                {
                    if let Some(queue_filters) = filters.get(&id).map(|filters| filters.iter()) {
                        for (filter_key, filter_value) in queue_filters {
                            session_table.add_row(row![
                                id,
                                original_name,
                                output_name,
                                format!("{filter_key}:{filter_value}")
                            ]);
                        }
                    }
                }

                (session.spec, session_table)
            })
        {
            println!("SQS Queue for {}:", sqs_session_spec.queue_consumer);
            sqs_session_table.printstd();
        }

        Ok(())
    }
}
