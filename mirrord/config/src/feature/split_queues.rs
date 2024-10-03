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
///         "queue_type": "Kafka",
///         "message_filter": {
///           "who": "*you$"
///         }
///       },
///       "fourth-queue": {
///         "queue_type": "Kafka",
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
pub struct SplitQueuesConfig(BTreeMap<QueueId, QueueFilter>);

impl SplitQueuesConfig {
    /// Returns whether this configuration contains any queue at all.
    pub fn is_set(&self) -> bool {
        !self.0.is_empty()
    }

    /// Out of the whole queue splitting config, get only the sqs queues.
    pub fn sqs(&self) -> impl '_ + Iterator<Item = (&'_ str, &'_ BTreeMap<String, String>)> {
        self.0.iter().filter_map(|(name, filter)| match filter {
            QueueFilter::Sqs { message_filter } => Some((name.as_str(), message_filter)),
            _ => None,
        })
    }

    /// Out of the whole queue splitting config, get only the kafka topics.
    pub fn kafka(&self) -> impl '_ + Iterator<Item = (&'_ str, &'_ BTreeMap<String, String>)> {
        self.0.iter().filter_map(|(name, filter)| match filter {
            QueueFilter::Kafka { message_filter } => Some((name.as_str(), message_filter)),
            _ => None,
        })
    }

    pub fn verify(
        &self,
        _context: &mut ConfigContext,
    ) -> Result<(), QueueSplittingVerificationError> {
        for (queue_name, filter) in &self.0 {
            let filter = match filter {
                QueueFilter::Sqs { message_filter } | QueueFilter::Kafka { message_filter } => {
                    message_filter
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

pub type QueueMessageFilter = BTreeMap<String, String>;

/// More queue types might be added in the future.
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, JsonSchema)]
#[serde(tag = "queue_type")]
pub enum QueueFilter {
    /// Amazon Simple Queue Service.
    #[serde(rename = "SQS")]
    Sqs {
        /// A filter is a mapping between message attribute names and regexes they should match.
        /// The local application will only receive messages that match **all** of the given
        /// patterns. This means, only messages that have **all** of the attributes in the
        /// filter, with values of those attributes matching the respective patterns.
        message_filter: QueueMessageFilter,
    },

    /// Kafka.
    #[serde(rename = "Kafka")]
    Kafka {
        /// A filter is a mapping between message header names and regexes they should match.
        /// The local application will only receive messages that match **all** of the given
        /// patterns. This means, only messages that have **all** of the headers in the
        /// filter, with values of those headers matching the respective patterns.
        message_filter: QueueMessageFilter,
    },

    /// When a newer client sends a new filter kind to an older operator, that does not yet know
    /// about that filter type, this is what that filter will be deserialized to.
    #[schemars(skip)]
    #[serde(other, skip_serializing)]
    Unknown,
}

impl CollectAnalytics for &SplitQueuesConfig {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        analytics.add("queue_count", self.0.len());
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

#[cfg(test)]
mod test {
    use super::QueueFilter;

    #[test]
    fn deserialize_known_queue_type() {
        let value = serde_json::json!({
            "queue_type": "Kafka",
            "message_filter": {
                "key": "value",
            },
        });

        let filter = serde_json::from_value::<QueueFilter>(value).unwrap();
        assert_eq!(
            filter,
            QueueFilter::Kafka {
                message_filter: [("key".to_string(), "value".to_string())].into()
            }
        );
    }

    #[test]
    fn deserialize_unknown_queue_type() {
        let value = serde_json::json!({
            "queue_type": "unknown",
            "message_filter": {
                "key": "value",
            }
        });

        let filter = serde_json::from_value::<QueueFilter>(value).unwrap();
        assert_eq!(filter, QueueFilter::Unknown);
    }
}
