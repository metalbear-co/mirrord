use core::slice::Iter;
use std::{
    collections::{BTreeMap, HashMap},
    ops::Not,
    path::PathBuf,
    time::Duration,
};

use mirrord_analytics::NullReporter;
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    LayerConfig, LayerFileConfig,
};
use mirrord_operator::{
    client::{NoClientCert, OperatorApi},
    crd::{MirrordOperatorSpec, MirrordSqsSession, QueueConsumer, QueueNameUpdate},
    types::LicenseInfoOwned,
};
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{row, Row, Table};
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

    /// The SQS information we want to display to the user is in a mix of different maps, and
    /// different parts of a CRD (some info is in the `crd.status`, while others are in
    /// `crd.spec`). This function digs into those different parts, skipping over pontential
    /// `None` in some fields that are optional.
    ///
    /// Returns the SQS session status rows keyed by consumer (the targeted resource, i.e. pod).
    #[tracing::instrument(level = Level::TRACE, ret)]
    fn sqs_rows(
        queues: Iter<MirrordSqsSession>,
        session_id: String,
        user: &String,
    ) -> Option<HashMap<QueueConsumer, Vec<Row>>> {
        /// The info we need to put in the rows when reporting the SQS status.
        struct QueueDisplayInfo<'a> {
            names: &'a BTreeMap<String, QueueNameUpdate>,
            consumer: &'a QueueConsumer,
            filters: &'a HashMap<String, BTreeMap<String, String>>,
        }

        let mut rows: HashMap<QueueConsumer, Vec<Row>> = HashMap::new();

        // Loop over the `MirrordSqsSession` crds to build the list of rows.
        for QueueDisplayInfo {
            names,
            consumer,
            filters,
        } in queues.filter_map(|queue| {
            // Dig into the `MirrordSqsSession` crd and get the meaningful parts.
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
        }) {
            // From the list of queue names, loop over them so we can match the `QueueId`
            // of a name with the `QueueId` of a filter.
            for (
                queue_id,
                QueueNameUpdate {
                    original_name,
                    output_name,
                },
            ) in names.iter()
            {
                // Basically `filter.queue_id == name.queue_id`.
                if let Some(filters_by_id) = filters.get(queue_id) {
                    // Loop over the filters and start building the rows.
                    for (filter_key, filter) in filters_by_id.iter() {
                        // Group rows by the queue `consumer`.
                        match rows.get_mut(consumer) {
                            Some(consumer_rows) => {
                                consumer_rows.push(row![
                                    session_id,
                                    queue_id,
                                    user,
                                    original_name,
                                    output_name,
                                    format!("{filter_key}:{filter}")
                                ]);
                            }
                            None => {
                                rows.insert(
                                    consumer.clone(),
                                    vec![row![
                                        session_id,
                                        queue_id,
                                        user,
                                        original_name,
                                        output_name,
                                        format!("{filter_key}:{filter}")
                                    ]],
                                );
                            }
                        }
                    }
                }
            }
        }

        // If it's empty, we don't want to display anything.
        rows.is_empty().not().then_some(rows)
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

        let mut sqs_rows: HashMap<QueueConsumer, Vec<Row>> = HashMap::new();

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

            if let Some(sqs_in_status) = session.sqs.as_ref().and_then(|sqs| {
                Self::sqs_rows(
                    sqs.iter(),
                    session.id.clone().unwrap_or_default(),
                    &session.user,
                )
            }) {
                sqs_rows.extend(sqs_in_status);
            }
        }

        sessions.printstd();
        println!();

        // The SQS queue statuses are grouped by queue consumer.
        for (sqs_consumer, sqs_row) in sqs_rows.into_iter() {
            let mut sqs_table = Table::new();
            sqs_table.add_row(row![
                "Session ID",
                "Queue ID",
                "User",
                "Original Name",
                "Output Name",
                "Filter",
            ]);

            println!("SQS Queue for {sqs_consumer}");

            for row in sqs_row {
                sqs_table.add_row(row);
            }

            sqs_table.printstd();
        }

        Ok(())
    }
}
