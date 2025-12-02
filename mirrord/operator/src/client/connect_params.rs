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

    /// Resource names of the database branches to use for the connection.
    #[serde(with = "mysql_branches_serde")]
    pub mysql_branch_names: Vec<String>,

    #[serde(with = "session_ci_info_serde")]
    pub session_ci_info: Option<SessionCiInfo>,
}

impl<'a> ConnectParams<'a> {
    pub fn new(
        config: &'a LayerConfig,
        branch_name: Option<String>,
        mysql_branch_names: Vec<String>,
        session_ci_info: Option<SessionCiInfo>,
    ) -> Self {
        Self {
            connect: true,
            on_concurrent_steal: config.feature.network.incoming.on_concurrent_steal.into(),
            profile: config.profile.as_deref(),
            kafka_splits: config.feature.split_queues.kafka().collect(),
            sqs_splits: config.feature.split_queues.sqs().collect(),
            branch_name,
            mysql_branch_names,
            session_ci_info,
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

mod session_ci_info_serde {
    use serde::Serializer;

    use crate::crd::session::SessionCiInfo;

    pub fn serialize<S>(
        session_ci_ifno: &Option<SessionCiInfo>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let as_json = serde_json::to_string(session_ci_ifno)
            .expect("serialization to memory should not fail");
        serializer.serialize_str(&as_json)
    }
}

impl fmt::Display for ConnectParams<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_string =
            serde_urlencoded::to_string(self).expect("serialization to memory should not fail");

        f.write_str(&as_string)
    }
}
