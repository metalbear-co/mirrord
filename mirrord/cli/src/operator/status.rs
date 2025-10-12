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
        MirrordKafkaSession, MirrordOperatorSpec, MirrordSqsSession, QueueConsumer,
        QueueNameUpdate, TopicNameUpdate,
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

    /// The Kafka information we want to display to the user is in a mix of different maps, and
    /// different parts of a CRD (some info is in the `crd.status`, while others are in
    /// `crd.spec`). This function digs into those different parts, skipping over potential
    /// `None` in some fields that are optional.
    ///
    /// Returns the Kafka session status rows keyed by consumer (the targeted resource, i.e. pod).
    #[tracing::instrument(level = Level::TRACE, ret)]
    fn kafka_rows(
        topics: Iter<MirrordKafkaSession>,
        session_id: String,
        user: &String,
    ) -> Option<HashMap<QueueConsumer, Vec<Row>>> {
        tracing::info!(
            "kafka_rows called with session_id: {}, user: {}",
            session_id,
            user
        );
        /// The info we need to put in the rows when reporting the Kafka status.
        struct TopicDisplayInfo<'a> {
            names: &'a BTreeMap<String, TopicNameUpdate>,
            consumer: &'a QueueConsumer,
            filters: &'a HashMap<String, BTreeMap<String, String>>,
        }

        let mut rows: HashMap<QueueConsumer, Vec<Row>> = HashMap::new();
        let topics_vec: Vec<_> = topics.collect();
        tracing::info!("Processing {} Kafka topics", topics_vec.len());

        for (index, topic) in topics_vec.iter().enumerate() {
            tracing::info!("Processing Kafka topic {}: {:?}", index, topic);

            let topic_display_info = match topic.status.as_ref() {
                Some(status) => {
                    tracing::info!("Topic {} has status: {:?}", index, status);
                    match status.get_split_details().as_ref() {
                        Some(split_details) => {
                            tracing::info!(
                                "Topic {} has split_details with {} topic_names",
                                index,
                                split_details.topic_names.len()
                            );
                            Some(TopicDisplayInfo {
                                names: &split_details.topic_names,
                                consumer: &topic.spec.topic_consumer,
                                filters: &topic.spec.topic_filters,
                            })
                        }
                        None => {
                            tracing::warn!("Topic {} has no split_details", index);
                            None
                        }
                    }
                }
                None => {
                    tracing::warn!("Topic {} has no status", index);
                    None
                }
            };

            if let Some(TopicDisplayInfo {
                names,
                consumer,
                filters,
            }) = topic_display_info
            {
                tracing::info!(
                    "Processing topic names: {} names, {} filters",
                    names.len(),
                    filters.len()
                );

                // From the list of topic names, loop over them so we can match the `TopicId`
                // of a name with the `TopicId` of a filter.
                for (
                    topic_id,
                    TopicNameUpdate {
                        original_name,
                        output_name,
                    },
                ) in names.iter()
                {
                    tracing::info!(
                        "Processing topic_id: {}, original_name: {}, output_name: {}",
                        topic_id,
                        original_name,
                        output_name
                    );

                    // Basically `filter.topic_id == name.topic_id`.
                    if let Some(filters_by_id) = filters.get(topic_id) {
                        tracing::info!(
                            "Found {} filters for topic_id: {}",
                            filters_by_id.len(),
                            topic_id
                        );

                        // Loop over the filters and start building the rows.
                        for (filter_key, filter) in filters_by_id.iter() {
                            tracing::info!(
                                "Adding row for consumer: {}, filter: {}:{}",
                                consumer,
                                filter_key,
                                filter
                            );

                            // Group rows by the topic `consumer`.
                            match rows.entry(consumer.clone()) {
                                Entry::Occupied(mut consumer_rows) => {
                                    consumer_rows.get_mut().push(row![
                                        session_id,
                                        topic_id,
                                        user,
                                        original_name,
                                        output_name,
                                        format!("{filter_key}:{filter}")
                                    ]);
                                }
                                Entry::Vacant(consumer_rows) => {
                                    consumer_rows.insert(vec![row![
                                        session_id,
                                        topic_id,
                                        user,
                                        original_name,
                                        output_name,
                                        format!("{filter_key}:{filter}")
                                    ]]);
                                }
                            }
                        }
                    } else {
                        tracing::warn!("No filters found for topic_id: {}", topic_id);
                    }
                }
            }
        }

        // If it's empty, we don't want to display anything.
        tracing::info!(
            "kafka_rows returning {} rows for {} consumers",
            rows.values().map(|v| v.len()).sum::<usize>(),
            rows.len()
        );
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

        tracing::info!(
            "Operator status retrieved with {} sessions",
            status.sessions.len()
        );

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
        let mut kafka_rows: HashMap<QueueConsumer, Vec<Row>> = HashMap::new();

        for (session_index, session) in status.sessions.iter().enumerate() {
            tracing::info!(
                "Processing session {}: id={:?}, target={}, user={}",
                session_index,
                session.id,
                session.target,
                session.user
            );

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

            // Check for SQS data
            if let Some(sqs) = session.sqs.as_ref() {
                tracing::info!("Session {} has {} SQS sessions", session_index, sqs.len());
                if let Some(sqs_in_status) = Self::sqs_rows(
                    sqs.iter(),
                    session.id.clone().unwrap_or_default(),
                    &session.user,
                ) {
                    tracing::info!(
                        "Session {} SQS processing returned {} consumers",
                        session_index,
                        sqs_in_status.len()
                    );
                    // Merge each session SQS into our map keyed by consumer.
                    for (consumer, rows) in sqs_in_status {
                        match sqs_rows.entry(consumer) {
                            Entry::Occupied(mut consumer_rows) => {
                                consumer_rows.get_mut().extend(rows)
                            }
                            Entry::Vacant(consumer_rows) => {
                                consumer_rows.insert(rows);
                            }
                        }
                    }
                } else {
                    tracing::warn!("Session {} SQS processing returned None", session_index);
                }
            } else {
                tracing::info!("Session {} has no SQS data", session_index);
            }

            // Check for Kafka data
            if let Some(kafka) = session.kafka.as_ref() {
                tracing::info!(
                    "Session {} has {} Kafka sessions",
                    session_index,
                    kafka.len()
                );
                if let Some(kafka_in_status) = Self::kafka_rows(
                    kafka.iter(),
                    session.id.clone().unwrap_or_default(),
                    &session.user,
                ) {
                    tracing::info!(
                        "Session {} Kafka processing returned {} consumers",
                        session_index,
                        kafka_in_status.len()
                    );
                    // Merge each session Kafka into our map keyed by consumer.
                    for (consumer, rows) in kafka_in_status {
                        match kafka_rows.entry(consumer) {
                            Entry::Occupied(mut consumer_rows) => {
                                consumer_rows.get_mut().extend(rows)
                            }
                            Entry::Vacant(consumer_rows) => {
                                consumer_rows.insert(rows);
                            }
                        }
                    }
                } else {
                    tracing::warn!("Session {} Kafka processing returned None", session_index);
                }
            } else {
                tracing::info!("Session {} has no Kafka data", session_index);
            }
        }

        sessions.printstd();
        println!();

        tracing::info!(
            "Final results: {} SQS consumers, {} Kafka consumers",
            sqs_rows.len(),
            kafka_rows.len()
        );

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

        // The Kafka topic statuses are grouped by topic consumer.
        if kafka_rows.is_empty() {
            tracing::info!("No Kafka rows to display");
        } else {
            tracing::info!("Displaying {} Kafka consumers", kafka_rows.len());
        }

        for (kafka_consumer, kafka_row) in kafka_rows {
            tracing::info!(
                "Displaying Kafka consumer: {} with {} rows",
                kafka_consumer,
                kafka_row.len()
            );
            let mut kafka_table = Table::new();
            kafka_table.add_row(row![
                "Session ID",
                "Topic ID",
                "User",
                "Original Name",
                "Output Name",
                "Filter",
            ]);

            println!("Kafka Topic for {kafka_consumer}");

            for row in kafka_row {
                kafka_table.add_row(row);
            }

            kafka_table.printstd();
        }

        Ok(())
    }
}
