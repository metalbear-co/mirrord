use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
};

use mirrord_config::{
    LayerConfig,
    feature::{
        network::incoming::ConcurrentSteal,
        queue_metadata_filter::{QueueMetadataFilterConfig, QueueType},
    },
};
use serde::{Serialize, Serializer};

use crate::crd::session::SessionCiInfo;

/// Ordered queue-split entries for one queue type.
///
/// The same queue id (including `*`) may appear more than once, so this cannot always be a plain
/// map. It serializes as a JSON object (`{"id": value}`) when every id is unique, which is exactly
/// what older operators expect, and only as a JSON array of `[id, value]` pairs when an id
/// repeats. New operators accept both shapes, and the array shape only shows up when the user
/// actually configured duplicate ids, which already requires a new operator.
#[derive(Debug, Clone)]
pub struct QueueSplits<'a, V>(pub Vec<(&'a str, V)>);

impl<V> QueueSplits<'_, V> {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<V> Default for QueueSplits<'_, V> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<'a, V> FromIterator<(&'a str, V)> for QueueSplits<'a, V> {
    fn from_iter<I: IntoIterator<Item = (&'a str, V)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl<V> Serialize for QueueSplits<'_, V>
where
    V: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seen = HashSet::with_capacity(self.0.len());
        let has_duplicates = self.0.iter().any(|(id, _)| !seen.insert(*id));

        if has_duplicates {
            serializer.collect_seq(self.0.iter().map(|(id, value)| (id, value)))
        } else {
            serializer.collect_map(self.0.iter().map(|(id, value)| (id, value)))
        }
    }
}

/// List-form metadata filters from `feature.split_queues`, keyed by queue type.
#[derive(Debug, Clone, Default)]
pub struct MetadataFilterSplits<'a>(pub Vec<(QueueType, &'a str, &'a QueueMetadataFilterConfig)>);

impl<'a> MetadataFilterSplits<'a> {
    fn for_type(&self, queue_type: QueueType) -> QueueSplits<'a, &'a QueueMetadataFilterConfig> {
        self.0
            .iter()
            .filter(|(kind, ..)| *kind == queue_type)
            .map(|(_, queue_id, filter)| (*queue_id, *filter))
            .collect()
    }
}

impl<'a> FromIterator<(QueueType, &'a str, &'a QueueMetadataFilterConfig)>
    for MetadataFilterSplits<'a>
{
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = (QueueType, &'a str, &'a QueueMetadataFilterConfig)>,
    {
        Self(iter.into_iter().collect())
    }
}

/// Query params for the operator connect request.
///
/// Built from user config in [`ConnectParams::new`]. Queue metadata filters are stored once, keyed
/// by [`QueueType`], because that matches `feature.split_queues`. Use this type in CLI and test
/// code.
///
/// Encoding to a connect URL goes through [`ConnectParamsQuery`]: the operator still reads
/// separate query keys per broker (`sqs_metadata_filters`, `kafka_metadata_filters`, …), so
/// serialization fans metadata filters back out by type before URL encoding.
///
/// You can use [`fmt::Display`] to produce the encoded query string.
#[derive(Debug)]
pub struct ConnectParams<'a> {
    /// Should always be true.
    pub connect: bool,
    /// Desired behavior on steal subscription conflict.
    pub on_concurrent_steal: Option<ConcurrentSteal>,
    /// Selected mirrord profile.
    pub profile: Option<&'a str>,

    pub kafka_splits: QueueSplits<'a, &'a BTreeMap<String, String>>,
    pub sqs_splits: QueueSplits<'a, &'a BTreeMap<String, String>>,
    pub rmq_splits: QueueSplits<'a, &'a BTreeMap<String, String>>,
    pub gcp_pubsub_splits: QueueSplits<'a, &'a BTreeMap<String, String>>,
    pub sqs_jq_filters: QueueSplits<'a, &'a str>,
    pub gcp_pubsub_jq_filters: QueueSplits<'a, &'a str>,
    pub azure_service_bus_splits: QueueSplits<'a, &'a BTreeMap<String, String>>,
    pub azure_service_bus_jq_filters: QueueSplits<'a, &'a str>,
    pub redis_pubsub_splits: QueueSplits<'a, &'a BTreeMap<String, String>>,
    pub temporal_splits: QueueSplits<'a, &'a BTreeMap<String, String>>,
    pub redis_pubsub_jq_filters: QueueSplits<'a, &'a str>,
    pub temporal_jq_filters: QueueSplits<'a, &'a str>,

    /// Metadata filters from list-form `feature.split_queues` entries: `(queue type, queue id,
    /// filter)`.
    pub metadata_filters: MetadataFilterSplits<'a>,

    /// User's current git branch name - may be an empty string if user is in detached head mode or
    /// another error occurred: this case handled by the operator
    pub branch_name: Option<String>,

    /// Per-dialect branch names (kept for backwards compatibility with older operators).
    pub mysql_branch_names: Vec<String>,
    pub pg_branch_names: Vec<String>,
    pub mongodb_branch_names: Vec<String>,

    /// Unified branch database names. Used by internal operator-to-operator communication
    /// (multi-cluster envoy). New operators accept both this and the per-dialect fields above.
    pub branch_db_names: Vec<String>,

    pub session_ci_info: Option<SessionCiInfo>,

    /// Multi-cluster: whether this is the default cluster for stateful operations.
    pub is_default_cluster: Option<bool>,

    /// Multi-cluster: SQS output queue names.
    /// Maps original queue names to their output queue names.
    /// All clusters use the same temp queue names for consistency.
    pub sqs_output_queues: HashMap<String, String>,

    /// Multi-cluster: RMQ output queue names.
    /// Maps original queue names to their output queue names.
    /// All clusters use the same temp queue names for consistency.
    pub rmq_output_queues: HashMap<String, String>,

    /// When set to `false`, forces a single-cluster session on a multi-cluster Primary.
    pub multi_cluster: Option<bool>,
    /// Multi-cluster: prefilled temporary resource names from the default cluster.
    /// The envoy reads these from the default cluster's Ready status and passes
    /// them to remote clusters so they reuse the same broker resources.
    pub output_tmp_resources: Vec<OutputTmpResource>,

    /// Key for this session
    pub key: Option<&'a str>,

    pub header_filter: Option<&'a str>,
}

/// Serde view of [`ConnectParams`] for the operator connect URL.
///
/// Field names here are query param names, not an internal API. The operator deserializes each
/// broker's queue filters from its own key (`sqs_splits`, `kafka_metadata_filters`, …) because
/// that is the connect protocol older CLIs and operators already speak. [`ConnectParams`] keeps a
/// single `metadata_filters` list instead; [`From`] on this type splits that list by
/// [`QueueType`] only for URL encoding.
///
/// Do not construct this directly - build [`ConnectParams`] and let [`Serialize`] or
/// [`fmt::Display`] convert it.
#[derive(Serialize)]
struct ConnectParamsQuery<'a> {
    pub connect: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_concurrent_steal: Option<ConcurrentSteal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<&'a str>,

    #[serde(with = "force_json_ser", skip_serializing_if = "QueueSplits::is_empty")]
    pub kafka_splits: QueueSplits<'a, &'a BTreeMap<String, String>>,

    #[serde(with = "force_json_ser", skip_serializing_if = "QueueSplits::is_empty")]
    pub sqs_splits: QueueSplits<'a, &'a BTreeMap<String, String>>,

    #[serde(with = "force_json_ser", skip_serializing_if = "QueueSplits::is_empty")]
    pub rmq_splits: QueueSplits<'a, &'a BTreeMap<String, String>>,

    #[serde(with = "force_json_ser", skip_serializing_if = "QueueSplits::is_empty")]
    pub gcp_pubsub_splits: QueueSplits<'a, &'a BTreeMap<String, String>>,

    #[serde(
        default,
        with = "force_json_ser",
        skip_serializing_if = "QueueSplits::is_empty"
    )]
    pub sqs_jq_filters: QueueSplits<'a, &'a str>,

    #[serde(
        default,
        with = "force_json_ser",
        skip_serializing_if = "QueueSplits::is_empty"
    )]
    pub gcp_pubsub_jq_filters: QueueSplits<'a, &'a str>,

    #[serde(with = "force_json_ser", skip_serializing_if = "QueueSplits::is_empty")]
    pub azure_service_bus_splits: QueueSplits<'a, &'a BTreeMap<String, String>>,

    #[serde(
        default,
        with = "force_json_ser",
        skip_serializing_if = "QueueSplits::is_empty"
    )]
    pub azure_service_bus_jq_filters: QueueSplits<'a, &'a str>,

    #[serde(with = "force_json_ser", skip_serializing_if = "QueueSplits::is_empty")]
    pub redis_pubsub_splits: QueueSplits<'a, &'a BTreeMap<String, String>>,

    #[serde(with = "force_json_ser", skip_serializing_if = "QueueSplits::is_empty")]
    pub temporal_splits: QueueSplits<'a, &'a BTreeMap<String, String>>,

    #[serde(
        default,
        with = "force_json_ser",
        skip_serializing_if = "QueueSplits::is_empty"
    )]
    pub redis_pubsub_jq_filters: QueueSplits<'a, &'a str>,

    #[serde(
        default,
        with = "force_json_ser",
        skip_serializing_if = "QueueSplits::is_empty"
    )]
    pub temporal_jq_filters: QueueSplits<'a, &'a str>,

    #[serde(with = "force_json_ser", skip_serializing_if = "QueueSplits::is_empty")]
    pub sqs_metadata_filters: QueueSplits<'a, &'a QueueMetadataFilterConfig>,

    #[serde(with = "force_json_ser", skip_serializing_if = "QueueSplits::is_empty")]
    pub kafka_metadata_filters: QueueSplits<'a, &'a QueueMetadataFilterConfig>,

    #[serde(with = "force_json_ser", skip_serializing_if = "QueueSplits::is_empty")]
    pub rmq_metadata_filters: QueueSplits<'a, &'a QueueMetadataFilterConfig>,

    #[serde(with = "force_json_ser", skip_serializing_if = "QueueSplits::is_empty")]
    pub gcp_pubsub_metadata_filters: QueueSplits<'a, &'a QueueMetadataFilterConfig>,

    #[serde(with = "force_json_ser", skip_serializing_if = "QueueSplits::is_empty")]
    pub azure_service_bus_metadata_filters: QueueSplits<'a, &'a QueueMetadataFilterConfig>,

    #[serde(with = "force_json_ser", skip_serializing_if = "QueueSplits::is_empty")]
    pub redis_pubsub_metadata_filters: QueueSplits<'a, &'a QueueMetadataFilterConfig>,

    #[serde(with = "force_json_ser", skip_serializing_if = "QueueSplits::is_empty")]
    pub temporal_metadata_filters: QueueSplits<'a, &'a QueueMetadataFilterConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,

    #[serde(with = "force_json_ser", skip_serializing_if = "Vec::is_empty")]
    pub mysql_branch_names: Vec<String>,
    #[serde(with = "force_json_ser", skip_serializing_if = "Vec::is_empty")]
    pub pg_branch_names: Vec<String>,
    #[serde(with = "force_json_ser", skip_serializing_if = "Vec::is_empty")]
    pub mongodb_branch_names: Vec<String>,

    #[serde(with = "force_json_ser", skip_serializing_if = "Vec::is_empty")]
    pub branch_db_names: Vec<String>,

    #[serde(with = "force_json_ser", skip_serializing_if = "Option::is_none")]
    pub session_ci_info: Option<SessionCiInfo>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_default_cluster: Option<bool>,

    #[serde(
        default,
        skip_serializing_if = "HashMap::is_empty",
        with = "output_queues_serde"
    )]
    pub sqs_output_queues: HashMap<String, String>,

    #[serde(
        default,
        skip_serializing_if = "HashMap::is_empty",
        with = "output_queues_serde"
    )]
    pub rmq_output_queues: HashMap<String, String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi_cluster: Option<bool>,

    #[serde(with = "force_json_ser", skip_serializing_if = "Vec::is_empty")]
    pub output_tmp_resources: Vec<OutputTmpResource>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<&'a str>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_filter: Option<&'a str>,
}

impl<'a> From<&ConnectParams<'a>> for ConnectParamsQuery<'a> {
    fn from(params: &ConnectParams<'a>) -> Self {
        Self {
            connect: params.connect,
            on_concurrent_steal: params.on_concurrent_steal,
            profile: params.profile,
            kafka_splits: params.kafka_splits.clone(),
            sqs_splits: params.sqs_splits.clone(),
            rmq_splits: params.rmq_splits.clone(),
            gcp_pubsub_splits: params.gcp_pubsub_splits.clone(),
            sqs_jq_filters: params.sqs_jq_filters.clone(),
            gcp_pubsub_jq_filters: params.gcp_pubsub_jq_filters.clone(),
            azure_service_bus_splits: params.azure_service_bus_splits.clone(),
            azure_service_bus_jq_filters: params.azure_service_bus_jq_filters.clone(),
            redis_pubsub_splits: params.redis_pubsub_splits.clone(),
            temporal_splits: params.temporal_splits.clone(),
            redis_pubsub_jq_filters: params.redis_pubsub_jq_filters.clone(),
            temporal_jq_filters: params.temporal_jq_filters.clone(),
            sqs_metadata_filters: params.metadata_filters.for_type(QueueType::Sqs),
            kafka_metadata_filters: params.metadata_filters.for_type(QueueType::Kafka),
            rmq_metadata_filters: params.metadata_filters.for_type(QueueType::Rmq),
            gcp_pubsub_metadata_filters: params.metadata_filters.for_type(QueueType::GcpPubSub),
            azure_service_bus_metadata_filters: params
                .metadata_filters
                .for_type(QueueType::AzureServiceBus),
            redis_pubsub_metadata_filters: params.metadata_filters.for_type(QueueType::RedisPubSub),
            temporal_metadata_filters: params.metadata_filters.for_type(QueueType::Temporal),
            branch_name: params.branch_name.clone(),
            mysql_branch_names: params.mysql_branch_names.clone(),
            pg_branch_names: params.pg_branch_names.clone(),
            mongodb_branch_names: params.mongodb_branch_names.clone(),
            branch_db_names: params.branch_db_names.clone(),
            session_ci_info: params.session_ci_info.clone(),
            is_default_cluster: params.is_default_cluster,
            sqs_output_queues: params.sqs_output_queues.clone(),
            rmq_output_queues: params.rmq_output_queues.clone(),
            multi_cluster: params.multi_cluster,
            output_tmp_resources: params.output_tmp_resources.clone(),
            key: params.key,
            header_filter: params.header_filter,
        }
    }
}

impl Serialize for ConnectParams<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ConnectParamsQuery::from(self).serialize(serializer)
    }
}

/// Same as TmpResourceEntry for serialization
/// in connect params. The envoy converts from the CRD type into this when
/// building the connect URL for remote clusters.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputTmpResource {
    pub queue_id: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub topic: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub subscription: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub channel: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub task_queue: BTreeMap<String, String>,
}

/// Per-dialect branch database names, used to keep the connect params
/// backwards-compatible with older operators that expect three separate fields.
#[derive(Debug, Default, Clone)]
pub struct BranchDbNames {
    pub pg: Vec<String>,
    pub mysql: Vec<String>,
    pub mongodb: Vec<String>,
    pub mssql: Vec<String>,
    pub redis: Vec<String>,
}

impl BranchDbNames {
    pub fn is_empty(&self) -> bool {
        self.pg.is_empty()
            && self.mysql.is_empty()
            && self.mongodb.is_empty()
            && self.mssql.is_empty()
            && self.redis.is_empty()
    }
}

impl<'a> ConnectParams<'a> {
    pub fn new(
        config: &'a LayerConfig,
        branch_name: Option<String>,
        branch_db_names: BranchDbNames,
        session_ci_info: Option<SessionCiInfo>,
        key: &'a str,
    ) -> Self {
        Self {
            connect: true,
            on_concurrent_steal: config.feature.network.incoming.on_concurrent_steal.into(),
            profile: config.profile.as_deref(),
            kafka_splits: config.feature.split_queues.kafka().collect(),
            rmq_splits: config.feature.split_queues.rmq().collect(),
            gcp_pubsub_splits: config.feature.split_queues.gcp_pubsub().collect(),
            sqs_splits: config.feature.split_queues.sqs().collect(),
            sqs_jq_filters: config.feature.split_queues.sqs_jq_filters().collect(),
            gcp_pubsub_jq_filters: config
                .feature
                .split_queues
                .gcp_pubsub_jq_filters()
                .collect(),
            azure_service_bus_splits: config.feature.split_queues.azure_service_bus().collect(),
            azure_service_bus_jq_filters: config
                .feature
                .split_queues
                .azure_service_bus_jq_filters()
                .collect(),
            redis_pubsub_splits: config.feature.split_queues.redis_pubsub().collect(),
            redis_pubsub_jq_filters: config
                .feature
                .split_queues
                .redis_pubsub_jq_filters()
                .collect(),
            temporal_splits: config.feature.split_queues.temporal().collect(),
            temporal_jq_filters: config.feature.split_queues.temporal_jq_filters().collect(),
            metadata_filters: config.feature.split_queues.metadata().collect(),
            branch_name,
            pg_branch_names: branch_db_names.pg,
            mysql_branch_names: branch_db_names.mysql,
            mongodb_branch_names: branch_db_names.mongodb,
            branch_db_names: branch_db_names
                .mssql
                .into_iter()
                .chain(branch_db_names.redis)
                .collect(),
            session_ci_info,
            is_default_cluster: None,
            sqs_output_queues: HashMap::new(),
            rmq_output_queues: HashMap::new(),
            multi_cluster: config.multi_cluster,
            output_tmp_resources: Vec::new(),
            key: Some(key),
            header_filter: config
                .feature
                .network
                .incoming
                .http_filter
                .header_filter
                .as_deref(),
        }
    }
}

/// This custom serialization, and the other modules for custom serialization,
/// are needed to override serde_urlencoded’s default query-string encoding so we can send complex
/// values as a single JSON-encoded string (and omit them when empty). Without them, the URL would
/// be encoded as repeated keys or nested key syntax that the operator API doesn’t expect.
///
/// ### Concrete example
///
/// Without the custom serializer, a `Vec<String>` would typically serialize into repeated params
/// like: `pg_branch_names=branch-1&pg_branch_names=branch-2`
///
/// But the operator expects a single JSON value, so the custom module emits:
/// `pg_branch_names = ["branch-1", "branch-2"]`
///
/// Which then gets URL-encoded by serde_urlencoded.
mod force_json_ser {

    use serde::{Serialize, Serializer};

    pub fn serialize<T, S>(queue_splits: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize,
    {
        let as_json =
            serde_json::to_string(queue_splits).expect("serialization to memory should not fail");
        serializer.serialize_str(as_json.as_str())
    }
}

mod output_queues_serde {
    use std::collections::HashMap;

    use serde::Serializer;

    pub fn serialize<S>(
        output_queues: &HashMap<String, String>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if output_queues.is_empty() {
            serializer.serialize_str("")
        } else {
            let as_json = serde_json::to_string(output_queues)
                .expect("serialization to memory should not fail");
            serializer.serialize_str(&as_json)
        }
    }
}

impl fmt::Display for ConnectParams<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_string =
            serde_urlencoded::to_string(self).expect("serialization to memory should not fail");

        f.write_str(&as_string)
    }
}

#[cfg(test)]
mod test {
    use mirrord_config::feature::queue_metadata_filter::QueueMetadataFilterConfig;

    use super::*;

    #[test]
    fn metadata_filters_serialize_to_per_broker_query_params() {
        let filter = QueueMetadataFilterConfig {
            metadata: Some(".*".to_string()),
            ..Default::default()
        };
        let params = ConnectParams {
            connect: true,
            on_concurrent_steal: None,
            profile: None,
            kafka_splits: Default::default(),
            sqs_splits: Default::default(),
            rmq_splits: Default::default(),
            gcp_pubsub_splits: Default::default(),
            sqs_jq_filters: Default::default(),
            gcp_pubsub_jq_filters: Default::default(),
            azure_service_bus_splits: Default::default(),
            azure_service_bus_jq_filters: Default::default(),
            redis_pubsub_splits: Default::default(),
            temporal_splits: Default::default(),
            redis_pubsub_jq_filters: Default::default(),
            temporal_jq_filters: Default::default(),
            metadata_filters: MetadataFilterSplits(vec![
                (QueueType::Sqs, "orders", &filter),
                (QueueType::Rmq, "*", &filter),
            ]),
            branch_name: None,
            mysql_branch_names: Vec::new(),
            pg_branch_names: Vec::new(),
            mongodb_branch_names: Vec::new(),
            branch_db_names: Vec::new(),
            session_ci_info: None,
            is_default_cluster: None,
            sqs_output_queues: HashMap::new(),
            rmq_output_queues: HashMap::new(),
            multi_cluster: None,
            output_tmp_resources: Vec::new(),
            key: None,
            header_filter: None,
        };

        let query = params.to_string();
        assert!(query.contains("sqs_metadata_filters="));
        assert!(query.contains("rmq_metadata_filters="));
        assert!(!query.contains("kafka_metadata_filters="));
    }
}
