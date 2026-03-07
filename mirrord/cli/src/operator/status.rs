use core::slice::Iter;
use std::{
    collections::{BTreeMap, HashMap, hash_map::Entry},
    ops::Not,
    path::PathBuf,
    time::Duration,
};

use mirrord_analytics::NullReporter;
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::{
    client::{NoClientCert, OperatorApi},
    crd::{
        MirrordOperatorSpec, MirrordSqsSession, QueueConsumer, QueueNameUpdate,
        kafka::MirrordKafkaEphemeralTopicSpec,
    },
    types::LicenseInfoOwned,
};
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{Row, Table, row};
use tracing::Level;

use crate::{CliResult, error::CliError, util::remove_proxy_env};

/// Handles the `mirrord operator status` command.
pub(super) struct StatusCommandHandler {
    /// Api to talk with session routes in the operator.
    operator_api: OperatorApi<NoClientCert>,
}

impl StatusCommandHandler {
    #[tracing::instrument(level = Level::TRACE, err)]
    pub(super) async fn new(config_file: Option<PathBuf>) -> CliResult<Self> {
        let mut progress = ProgressTracker::from_env("Operator Status");

        let mut cfg_context =
            ConfigContext::default().override_env_opt(LayerConfig::FILE_PATH_ENV, config_file);
        let layer_config = LayerConfig::resolve(&mut cfg_context)?;

        if !layer_config.use_proxy {
            remove_proxy_env();
        }

        let mut status_progress = progress.subtask("fetching status");
        let api = OperatorApi::try_new(&layer_config, &mut NullReporter::default(), &progress)
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

    /// The Kafka information we want to display to the user is in the MirrordKafkaEphemeralTopic
    /// CRDs. This function extracts the topic information and creates display rows.
    ///
    /// Returns the Kafka topic status rows keyed by consumer (the targeted resource, i.e. pod).
    #[tracing::instrument(level = Level::TRACE, ret)]
    fn kafka_rows(
        topics: Iter<MirrordKafkaEphemeralTopicSpec>,
        session_id: String,
        user: &String,
        consumer: &String,
    ) -> Option<HashMap<String, Vec<Row>>> {
        let mut rows: HashMap<String, Vec<Row>> = HashMap::new();

        // Loop over the `MirrordKafkaEphemeralTopic` crds to build the list of rows.
        for topic_spec in topics {
            let topic_name = &topic_spec.name;
            let client_config = &topic_spec.client_config;

            let topic_type = if topic_name.contains("-fallback-") {
                "Fallback"
            } else {
                "Filtered"
            };

            // Group rows by the consumer (target).
            match rows.entry(consumer.clone()) {
                Entry::Occupied(mut consumer_rows) => {
                    consumer_rows.get_mut().push(row![
                        session_id,
                        topic_name,
                        user,
                        client_config,
                        topic_type,
                    ]);
                }
                Entry::Vacant(consumer_rows) => {
                    consumer_rows.insert(vec![row![
                        session_id,
                        topic_name,
                        user,
                        client_config,
                        topic_type,
                    ]]);
                }
            }
        }

        // If it's empty, we don't want to display anything.
        rows.is_empty().not().then_some(rows)
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
                composite_key,
                QueueNameUpdate {
                    original_name,
                    output_name,
                },
            ) in names.iter()
            {
                // Parse the composite key format "queue_id::input_name" to extract queue_id
                let queue_id = composite_key
                    .split_once("::")
                    .map(|(id, _)| id)
                    .unwrap_or(composite_key.as_str());

                // Match with filters by queue_id, or use wildcard "*" if present
                let filters_by_id = filters.get(queue_id).or_else(|| filters.get("*"));

                if let Some(filters_by_id) = filters_by_id {
                    // Loop over the filters and start building the rows.
                    for (filter_key, filter) in filters_by_id.iter() {
                        // Group rows by the queue `consumer`.
                        match rows.entry(consumer.clone()) {
                            Entry::Occupied(mut consumer_rows) => {
                                consumer_rows.get_mut().push(row![
                                    session_id,
                                    queue_id,
                                    user,
                                    original_name,
                                    output_name,
                                    format!("{filter_key}:{filter}")
                                ]);
                            }
                            Entry::Vacant(consumer_rows) => {
                                consumer_rows.insert(vec![row![
                                    session_id,
                                    queue_id,
                                    user,
                                    original_name,
                                    output_name,
                                    format!("{filter_key}:{filter}")
                                ]]);
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

                for copy_target_entry_compat in copy_targets {
                    let copy_target_entry = copy_target_entry_compat.to_copy_target_entry();
                    copy_targets_table.add_row(row![
                        copy_target_entry.copy_target.spec.target.to_string(),
                        copy_target_entry
                            .copy_target
                            .metadata
                            .namespace
                            .as_deref()
                            .unwrap_or_default(),
                        copy_target_entry.pod_name,
                        if copy_target_entry.copy_target.spec.scale_down {
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

        status.statistics.as_ref().and_then(|statistics| {
            println!("Operator Daily Users: {}", statistics.dau);
            println!("Operator Monthly Users: {}", statistics.mau);

            println!(
                "Operator Concurrent CI Sessions: {}",
                statistics.active_ci_sessions_count?
            );

            Some(())
        });

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
        let mut kafka_rows: HashMap<String, Vec<Row>> = HashMap::new();

        for session in &status.sessions {
            let locked_ports = session
                .locked_ports
                .as_deref()
                .map(|ports| {
                    ports
                        .iter()
                        .map(|locked_port_compat| {
                            let locked_port = locked_port_compat.to_locked_port();
                            format!(
                                "Port: {}, Type: {}{}",
                                locked_port.port,
                                locked_port.kind,
                                locked_port
                                    .filter
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
                // Merge each session SQS into our map keyed by consumer.
                for (consumer, rows) in sqs_in_status {
                    match sqs_rows.entry(consumer) {
                        Entry::Occupied(mut consumer_rows) => consumer_rows.get_mut().extend(rows),
                        Entry::Vacant(consumer_rows) => {
                            consumer_rows.insert(rows);
                        }
                    }
                }
            }

            if let Some(kafka_in_status) = session.kafka.as_ref().and_then(|kafka| {
                Self::kafka_rows(
                    kafka.iter(),
                    session.id.clone().unwrap_or_default(),
                    &session.user,
                    &session.target,
                )
            }) {
                kafka_rows.extend(kafka_in_status);
            }
        }

        sessions.printstd();
        println!();

        // Multi-cluster sessions are only on Primary cluster.
        if let Some(mc_sessions) = status.multi_cluster_sessions.as_ref() {
            println!("Multi-Cluster Sessions:");
            let mut mc_table = Table::new();

            mc_table.add_row(row![
                "Session ID",
                "Target",
                "Namespace",
                "User",
                "Clusters",
                "Phase",
                "Session Duration"
            ]);

            for mc_session in mc_sessions {
                mc_table.add_row(row![
                    &mc_session.id,
                    &mc_session.target,
                    &mc_session.namespace,
                    &mc_session.user,
                    mc_session.clusters.join(", "),
                    &mc_session.phase,
                    humantime::format_duration(Duration::from_secs(mc_session.duration_secs)),
                ]);
            }

            mc_table.printstd();
            println!();
        }

        // The SQS queue statuses are grouped by queue consumer.
        for (sqs_consumer, sqs_row) in sqs_rows {
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

        // The Kafka topic statuses are grouped by consumer (target).
        for (_, kafka_row) in kafka_rows {
            let mut kafka_table = Table::new();
            kafka_table.add_row(row![
                "Session ID",
                "Topic Name",
                "User",
                "Client Config",
                "Type",
            ]);

            for row in kafka_row {
                kafka_table.add_row(row);
            }

            kafka_table.printstd();
        }

        Ok(())
    }
}
