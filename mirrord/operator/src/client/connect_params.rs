use std::{
    collections::{BTreeMap, HashMap},
    fmt,
};

use mirrord_config::{feature::network::incoming::ConcurrentSteal, LayerConfig};
use serde::Serialize;

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
}

impl<'a> ConnectParams<'a> {
    pub fn new(config: &'a LayerConfig, branch_name: String) -> Self {
        Self {
            connect: true,
            on_concurrent_steal: config.feature.network.incoming.on_concurrent_steal.into(),
            profile: config.profile.as_deref(),
            kafka_splits: config.feature.split_queues.kafka().collect(),
            sqs_splits: config.feature.split_queues.sqs().collect(),
            branch_name
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

impl fmt::Display for ConnectParams<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_string =
            serde_urlencoded::to_string(self).expect("serialization to memory should not fail");

        f.write_str(&as_string)
    }
}
