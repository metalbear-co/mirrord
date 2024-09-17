use std::collections::BTreeMap;

use fancy_regex::Regex;
use mirrord_analytics::{Analytics, CollectAnalytics};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
///       "third-queue": {
///         "queue_type": "kafka_topic",
///         "message_filter": {
///           "who": "*you$"
///         }
///       },
///       "fourth-queue": {
///         "queue_type": "kafka_topic",
///         "message_filter": {
///           "wows": "so wows",
///           "coolz": "^very .*"
///         }
///       },
///     }
///   }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize, Default)]
pub struct SplitQueuesConfig(pub Option<BTreeMap<QueueId, QueueFilter>>);

impl SplitQueuesConfig {
    /// Returns whether this configuration contains any queue at all.
    pub fn is_set(&self) -> bool {
        self.0
            .as_ref()
            .map(|map| !map.is_empty())
            .unwrap_or_default()
    }

    /// Out of the whole queue splitting config, get only the sqs queues.
    pub fn sqs(&self) -> impl '_ + Iterator<Item = (&'_ str, &'_ BTreeMap<String, String>)> {
        self.0
            .iter()
            .flatten()
            .filter_map(|(name, filter)| match filter {
                QueueFilter::Sqs(filter) => Some((name.as_str(), filter)),
                _ => None,
            })
    }

    /// Out of the whole queue splitting config, get only the kafka topics.
    pub fn kafka(&self) -> impl '_ + Iterator<Item = (&'_ str, &'_ BTreeMap<String, String>)> {
        self.0
            .iter()
            .flatten()
            .filter_map(|(name, filter)| match filter {
                QueueFilter::Kafka(filter) => Some((name.as_str(), filter)),
                _ => None,
            })
    }

    pub fn verify(
        &self,
        context: &mut ConfigContext,
    ) -> Result<(), QueueSplittingVerificationError> {
        let Some(filters) = self.0.as_ref() else {
            return Ok(());
        };

        for (queue_name, filter) in filters {
            let filter = match filter {
                QueueFilter::Sqs(filter) | QueueFilter::Kafka(filter) => {
                    if filter.is_empty() {
                        context.add_warning(format!("Message filter for queue {queue_name} is empty and will match all messages."));
                    }

                    filter
                }
                QueueFilter::Unknown => {
                    return Err(QueueSplittingVerificationError::UnknownQueueType(
                        queue_name.clone(),
                    ));
                }
            };

            for (name, pattern) in filter {
                Regex::new(pattern).map_err(|error| {
                    QueueSplittingVerificationError::InvalidRegex(
                        queue_name.clone(),
                        name.clone(),
                        error,
                    )
                })?;
            }
        }

        Ok(())
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

/// More queue types might be added in the future.
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, JsonSchema)]
#[serde(tag = "queue_type", content = "message_filter")]
pub enum QueueFilter {
    /// Amazon Simple Queue Service.
    ///
    /// A filter is a mapping between message attribute names and regexes they should match.
    /// The local application will only receive messages that match **all** of the given patterns.
    /// This means, only messages that have **all** of the attributes in the filter,
    /// with values of those attributes matching the respective patterns.
    #[serde(rename = "SQS")]
    Sqs(BTreeMap<String, String>),

    /// Kafka.
    ///
    /// A filter is a mapping between message header names and regexes matched against their
    /// values. The local application will only receive messages that match **all** of the
    /// given patterns. This means, only messages that have **all** of the headers in the
    /// filter, with values of those headers matching the respective patterns.
    #[serde(rename = "kafka")]
    Kafka(BTreeMap<String, String>),

    /// When a newer client sends a new filter kind to an older operator, that does not yet know
    /// about that filter type, this is what that filter will be deserialized to.
    #[schemars(skip)]
    #[serde(other, skip_serializing)]
    Unknown,
}

impl CollectAnalytics for &SplitQueuesConfig {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        analytics.add(
            "queue_count",
            self.0
                .as_ref()
                .map(|mapping| mapping.len())
                .unwrap_or_default(),
        );
        analytics.add("sqs_queue_count", self.sqs().count());
        analytics.add("kafka_queue_count", self.kafka().count());
    }
}

#[derive(Error, Debug)]
pub enum QueueSplittingVerificationError {
    #[error("{0}: unknown queue type")]
    UnknownQueueType(String),
    #[error("{0}.message_filter.{1}: failed to parse regular expression ({2})")]
    InvalidRegex(String, String, fancy_regex::Error),
}
