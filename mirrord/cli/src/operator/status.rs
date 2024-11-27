use core::slice::Iter;
use std::{
    collections::{BTreeMap, HashMap},
    ops::Not,
    path::PathBuf,
    time::Duration,
};

use kube::{api::ListParams, Api, Resource};
use mirrord_analytics::NullReporter;
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    LayerConfig, LayerFileConfig,
};
use mirrord_operator::{
    client::{NoClientCert, OperatorApi},
    crd::{
        is_session_ready, MirrordOperatorSpec, MirrordSqsSession, QueueConsumer, QueueId,
        QueueNameUpdate,
    },
    types::LicenseInfoOwned,
};
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{row, Row, Table};
use tracing::{Instrument, Level};

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

    /// SQS rows keyed by consumer (the targeted resource, i.e. pod).
    fn sqs_rows(
        queues: Iter<MirrordSqsSession>,
        session_id: &String,
        user: &String,
    ) -> HashMap<String, Vec<Row>> {
        struct QueueDisplayInfo<'a> {
            names: &'a BTreeMap<String, QueueNameUpdate>,
            consumer: &'a QueueConsumer,
            filters: &'a HashMap<String, BTreeMap<String, String>>,
        }

        queues
            .filter_map(|queue| {
                Some(QueueDisplayInfo {
                    names: &queue
                        .status
                        .as_ref()?
                        .get_split_details()
                        .as_ref()?
                        .queue_names,
                    consumer: &queue.spec.queue_consumer,
                    filters: &queue.spec.queue_filters,
                })
            })
            .fold(
                HashMap::new(),
                |mut rows,
                 QueueDisplayInfo {
                     names,
                     consumer,
                     filters,
                 }| {
                    names.iter().fold(
                        Vec::new(),
                        |row_by_name,
                         (
                            queue_id,
                            QueueNameUpdate {
                                original_name,
                                output_name,
                            },
                        )| {
                            if let Some(filters_by_id) = filters.get(queue_id) {
                                filters_by_id.iter().fold(
                                    Vec::new(),
                                    |mut fina, (filter_key, filter)| {
                                        fina.insert(
                                            consumer.name.clone(),
                                            vec![row![
                                                session_id,
                                                queue_id,
                                                user,
                                                original_name,
                                                output_name,
                                                format!("{filter_key}:{filter}")
                                            ]],
                                        );

                                        fina
                                    },
                                )
                            }

                            row_by_name
                        },
                    );

                    rows
                },
            )

        // for queue in queues {
        //     for (
        //         queue_id,
        //         QueueNameUpdate {
        //             original_name,
        //             output_name,
        //         },
        //     ) in queue
        //         .status
        //         .as_ref()?
        //         .get_split_details()?
        //         .queue_names
        //         .iter()
        //     {
        //         for (filter_key, filter) in queue.spec.queue_filters.get(queue_id)? {
        //             rows.push(row![
        //                 session_id,
        //                 queue_id,
        //                 user,
        //                 original_name,
        //                 output_name,
        //                 format!("{filter_key}:{filter}")
        //             ]);
        //         }
        //     }
        // }
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

        let mut sqs_rows = None;
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

            sqs_rows = session.sqs.as_ref().map(|sqs| Self::sqs_rows(sqs.iter()))
        }

        sessions.printstd();
        println!();

        if let Some(sqs) = sqs_rows.map(|rows| rows.into_iter()) {
            let mut sqs_table = Table::new();
            sqs_table.add_row(row![
                "Session ID",
                "Queue ID",
                "User",
                "Original Name",
                "Output Name",
                "Filter",
            ]);

            for (session, row) in sqs {
                sqs_table.add_row(row.first().cloned().unwrap());
            }

            sqs_table.printstd();
        }

        // let sqs_sessions_api = Api::<MirrordSqsSession>::all(api.client().clone());
        // for (session_id, consumer, sqs_session_table) in sqs_sessions_api
        //     .list(&ListParams::default())
        //     .await
        //     .map_err(CliError::ListSqsSessions)?
        //     .into_iter()
        //     .filter(|session| is_session_ready(Some(session)))
        //     .filter_map(|session| {
        //         Some((
        //             session
        //                 .status
        //                 .as_ref()?
        //                 .get_split_details()?
        //                 .clone()
        //                 .queue_names
        //                 .into_iter(),
        //             session,
        //         ))
        //     })
        //     .map(|(split_details, session)| {
        //         let mut session_table = Table::new();
        //         session_table.add_row(row!["Queue Id", "Original Name", "Output Name",
        // "Filter"]);

        //         let filters = &session.spec.queue_filters;
        //         println!("{session:#?}");

        //         for (
        //             id,
        //             QueueNameUpdate {
        //                 output_name,
        //                 original_name,
        //             },
        //         ) in split_details
        //         {
        //             if let Some(queue_filters) = filters.get(&id).map(|filters| filters.iter()) {
        //                 for (filter_key, filter_value) in queue_filters {
        //                     session_table.add_row(row![
        //                         id,
        //                         original_name,
        //                         output_name,
        //                         format!("{filter_key}:{filter_value}")
        //                     ]);
        //                 }
        //             }
        //         }

        //         let session_id = session
        //             .meta()
        //             .labels
        //             .as_ref()
        //             .and_then(|labels| labels.get("mirrord-session").cloned());
        //         (session_id, session.spec.queue_consumer, session_table)
        //     })
        // {
        //     println!(
        //         "SQS Queue Session {} for {}:",
        //         session_id.unwrap_or_default(),
        //         consumer
        //     );
        //     sqs_session_table.printstd();
        // }

        Ok(())
    }
}
