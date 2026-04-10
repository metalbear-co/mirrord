use std::{
    collections::{BTreeMap, HashMap},
    fmt,
};

use mirrord_config::{LayerConfig, feature::network::incoming::ConcurrentSteal};
use serde::Serialize;

use crate::crd::session::SessionCiInfo;

/// Query params for the operator connect request.
///
/// You can use the [`fmt::Display`] to get a properly encoded query string.
#[derive(Serialize)]
pub struct ConnectParams<'a> {
    /// Should always be true.
    pub connect: bool,
    /// Desired behavior on steal subscription conflict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_concurrent_steal: Option<ConcurrentSteal>,
    /// Selected mirrord profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<&'a str>,

    #[serde(with = "force_json_ser", skip_serializing_if = "HashMap::is_empty")]
    pub kafka_splits: HashMap<&'a str, &'a BTreeMap<String, String>>,

    #[serde(with = "force_json_ser", skip_serializing_if = "HashMap::is_empty")]
    pub sqs_splits: HashMap<&'a str, &'a BTreeMap<String, String>>,

    #[serde(with = "force_json_ser", skip_serializing_if = "HashMap::is_empty")]
    pub rmq_splits: HashMap<&'a str, &'a BTreeMap<String, String>>,

    #[serde(with = "force_json_ser", skip_serializing_if = "HashMap::is_empty")]
    pub gcp_pubsub_splits: HashMap<&'a str, &'a BTreeMap<String, String>>,

    #[serde(
        default,
        with = "force_json_ser",
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub sqs_jq_filters: HashMap<&'a str, &'a str>,

    /// User's current git branch name - may be an empty string if user is in detached head mode or
    /// another error occurred: this case handled by the operator
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,

    /// Per-dialect branch names (kept for backwards compatibility with older operators).
    #[serde(with = "force_json_ser", skip_serializing_if = "Vec::is_empty")]
    pub mysql_branch_names: Vec<String>,
    #[serde(with = "force_json_ser", skip_serializing_if = "Vec::is_empty")]
    pub pg_branch_names: Vec<String>,
    #[serde(with = "force_json_ser", skip_serializing_if = "Vec::is_empty")]
    pub mongodb_branch_names: Vec<String>,

    /// Unified branch database names. Used by internal operator-to-operator communication
    /// (multi-cluster envoy). New operators accept both this and the per-dialect fields above.
    #[serde(with = "force_json_ser", skip_serializing_if = "Vec::is_empty")]
    pub branch_db_names: Vec<String>,

    #[serde(with = "force_json_ser", skip_serializing_if = "Option::is_none")]
    pub session_ci_info: Option<SessionCiInfo>,

    /// Multi-cluster: whether this is the default cluster for stateful operations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_default_cluster: Option<bool>,

    /// Multi-cluster: SQS output queue names.
    /// Maps original queue names to their output queue names.
    /// All clusters use the same temp queue names for consistency.
    #[serde(
        default,
        skip_serializing_if = "HashMap::is_empty",
        with = "output_queues_serde"
    )]
    pub sqs_output_queues: HashMap<String, String>,

    /// Multi-cluster: RMQ output queue names.
    /// Maps original queue names to their output queue names.
    /// All clusters use the same temp queue names for consistency.
    #[serde(
        default,
        skip_serializing_if = "HashMap::is_empty",
        with = "output_queues_serde"
    )]
    pub rmq_output_queues: HashMap<String, String>,

    /// Key for this session
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<&'a str>,
}

/// Per-dialect branch database names, used to keep the connect params
/// backwards-compatible with older operators that expect three separate fields.
#[derive(Debug, Default, Clone)]
pub struct BranchDbNames {
    pub pg: Vec<String>,
    pub mysql: Vec<String>,
    pub mongodb: Vec<String>,
    pub mssql: Vec<String>,
}

impl BranchDbNames {
    pub fn is_empty(&self) -> bool {
        self.pg.is_empty()
            && self.mysql.is_empty()
            && self.mongodb.is_empty()
            && self.mssql.is_empty()
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
            branch_name,
            pg_branch_names: branch_db_names.pg,
            mysql_branch_names: branch_db_names.mysql,
            mongodb_branch_names: branch_db_names.mongodb,
            branch_db_names: branch_db_names.mssql,
            session_ci_info,
            is_default_cluster: None,
            sqs_output_queues: HashMap::new(),
            rmq_output_queues: HashMap::new(),
            key: Some(key),
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
