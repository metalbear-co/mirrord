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

    #[serde(with = "queue_splits_serde")]
    pub kafka_splits: HashMap<&'a str, &'a BTreeMap<String, String>>,

    #[serde(with = "queue_splits_serde")]
    pub sqs_splits: HashMap<&'a str, &'a BTreeMap<String, String>>,
    /// User's current git branch name - may be an empty string if user is in detached head mode or
    /// another error occurred: this case handled by the operator
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,

    /// Resource names of the MongoDB branch databases to use for the connection.
    #[serde(with = "mongodb_branches_serde")]
    pub mongodb_branch_names: Vec<String>,

    /// Resource names of the database branches to use for the connection.
    #[serde(with = "mysql_branches_serde")]
    pub mysql_branch_names: Vec<String>,

    /// Resource names of the PostgreSQL branch databases to use for the connection.
    #[serde(with = "pg_branches_serde")]
    pub pg_branch_names: Vec<String>,

    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "session_ci_info_serde"
    )]
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
        with = "sqs_output_queues_serde"
    )]
    pub sqs_output_queues: HashMap<String, String>,

    /// Key for this session
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<&'a str>,
}

impl<'a> ConnectParams<'a> {
    pub fn new(
        config: &'a LayerConfig,
        branch_name: Option<String>,
        mongodb_branch_names: Vec<String>,
        mysql_branch_names: Vec<String>,
        pg_branch_names: Vec<String>,
        session_ci_info: Option<SessionCiInfo>,
        key: &'a str,
    ) -> Self {
        Self {
            connect: true,
            on_concurrent_steal: config.feature.network.incoming.on_concurrent_steal.into(),
            profile: config.profile.as_deref(),
            kafka_splits: config.feature.split_queues.kafka().collect(),
            sqs_splits: config.feature.split_queues.sqs().collect(),
            branch_name,
            mongodb_branch_names,
            mysql_branch_names,
            pg_branch_names,
            session_ci_info,
            is_default_cluster: None,          // Only used in multi-cluster
            sqs_output_queues: HashMap::new(), // Only used in multi-cluster
            key: Some(key),
        }
    }
}

mod queue_splits_serde {
    use std::collections::{BTreeMap, HashMap};

    use serde::Serializer;

    pub fn serialize<S>(
        queue_splits: &HashMap<&str, &BTreeMap<String, String>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if queue_splits.is_empty() {
            return serializer.serialize_none();
        }

        let as_json =
            serde_json::to_string(queue_splits).expect("serialization to memory should not fail");
        serializer.serialize_str(as_json.as_str())
    }
}

mod mongodb_branches_serde {
    use serde::Serializer;

    pub fn serialize<S>(mongodb_branches: &Vec<String>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if mongodb_branches.is_empty() {
            serializer.serialize_none()
        } else {
            let as_json = serde_json::to_string(mongodb_branches)
                .expect("serialization to memory should not fail");
            serializer.serialize_str(&as_json)
        }
    }
}

mod mysql_branches_serde {
    use serde::Serializer;

    pub fn serialize<S>(mysql_branches: &Vec<String>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if mysql_branches.is_empty() {
            serializer.serialize_none()
        } else {
            let as_json = serde_json::to_string(mysql_branches)
                .expect("serialization to memory should not fail");
            serializer.serialize_str(&as_json)
        }
    }
}

mod pg_branches_serde {
    use serde::Serializer;

    pub fn serialize<S>(pg_branches: &Vec<String>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if pg_branches.is_empty() {
            serializer.serialize_none()
        } else {
            let as_json = serde_json::to_string(pg_branches)
                .expect("serialization to memory should not fail");
            serializer.serialize_str(&as_json)
        }
    }
}

mod session_ci_info_serde {
    use serde::Serializer;

    use crate::crd::session::SessionCiInfo;

    pub fn serialize<S>(
        session_ci_info: &Option<SessionCiInfo>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let as_json = serde_json::to_string(session_ci_info)
            .expect("serialization to memory should not fail");
        serializer.serialize_str(&as_json)
    }
}

mod sqs_output_queues_serde {
    use std::collections::HashMap;

    use serde::Serializer;

    pub fn serialize<S>(
        sqs_output_queues: &HashMap<String, String>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if sqs_output_queues.is_empty() {
            serializer.serialize_str("")
        } else {
            let as_json = serde_json::to_string(sqs_output_queues)
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
