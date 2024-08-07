use std::collections::{BTreeMap, HashMap};

use mirrord_analytics::{Analytics, CollectAnalytics};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::{ConfigContext, FromMirrordConfig, MirrordConfig};

pub type QueueId = String;

/// ```json
/// {
///   "feature": {
///     "split_queues": {
///       "first-queue": {
///         "queue_type": "SQS",
///         "message_filter": {
///           "wows": "so wows",
///           "coolz": "^very .*"
///         }
///       },
///       "second-queue": {
///         "queue_type": "SQS",
///         "message_filter": {
///           "who": "*you$"
///         }
///       },
///     }
///   }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Deserialize, Default)]
pub struct SplitQueuesConfig(pub Option<BTreeMap<QueueId, QueueFilter>>);

impl SplitQueuesConfig {
    pub fn is_set(&self) -> bool {
        self.0.is_some()
    }

    /// Out of the whole queue splitting config, get only the sqs queues.
    pub fn get_sqs_filter(&self) -> Option<HashMap<String, SqsMessageFilter>> {
        self.0.as_ref().map(BTreeMap::iter).map(|filters| {
            filters
                // When there are more variants of QueueFilter, change this to a `filter_map`.
                .map(|(queue_id, queue_filter)| match queue_filter {
                    QueueFilter::Sqs(filter_mapping) => (queue_id.clone(), filter_mapping.clone()),
                })
                .collect()
        })
    }
}

impl MirrordConfig for SplitQueuesConfig {
    type Generated = Self;

    fn generate_config(
        self,
        _context: &mut ConfigContext,
    ) -> crate::config::Result<Self::Generated> {
        Ok(self)
    }
}

impl FromMirrordConfig for SplitQueuesConfig {
    type Generator = Self;
}

pub type MessageAttributeName = String;
pub type AttributeValuePattern = String;

/// A filter is a mapping between message attribute names and regexes they should match.
/// The local application will only receive messages that match **all** of the given patterns.
/// This means, only messages that have **all** the `MessageAttributeName`s in the filter,
/// with values of those attributes matching the respective `AttributeValuePattern`.
pub type SqsMessageFilter = BTreeMap<MessageAttributeName, AttributeValuePattern>;

/// More queue types might be added in the future.
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, JsonSchema)]
#[serde(tag = "queue_type", content = "message_filter")]
pub enum QueueFilter {
    /// Amazon Simple Queue Service.
    #[serde(rename = "SQS")]
    Sqs(SqsMessageFilter),
}

impl CollectAnalytics for &SplitQueuesConfig {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        analytics.add(
            "queue_count",
            self.0
                .as_ref()
                .map(|mapping| mapping.len())
                .unwrap_or_default(),
        )
    }
}
