use std::collections::BTreeMap;

use fancy_regex::Regex;
use jaq_all::load::FileReportsDisp;
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
///           "coolz": "^very"
///         }
///       },
///       "second-queue": {
///         "queue_type": "SQS",
///         "message_filter": {
///           "who": "you$"
///         }
///       },
///       "third-queue": {
///         "queue_type": "Kafka",
///         "message_filter": {
///           "who": "you$"
///         }
///       },
///       "fourth-queue": {
///         "queue_type": "Kafka",
///         "message_filter": {
///           "wows": "so wows",
///           "coolz": "^very"
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

    /// Get all the SQS queue ids from the config.
    pub fn sqs_queues(&self) -> impl '_ + Iterator<Item = &'_ str> {
        self.0.iter().filter_map(|(name, filter)| match filter {
            QueueFilter::Sqs { .. } => Some(name.as_str()),
            _ => None,
        })
    }

    /// Out of the whole queue splitting config, get only the sqs message attribute filters.
    pub fn sqs(&self) -> impl '_ + Iterator<Item = (&'_ str, &'_ QueueMessageFilter)> {
        self.0.iter().filter_map(|(name, filter)| match filter {
            QueueFilter::Sqs {
                message_filter: Some(message_filter),
                ..
            } => Some((name.as_str(), message_filter)),
            _ => None,
        })
    }

    /// Out of the whole queue splitting config, get only the sqs jq filters.
    pub fn sqs_jq_filters(&self) -> impl '_ + Iterator<Item = (&'_ str, &str)> {
        self.0.iter().filter_map(|(name, filter)| match filter {
            QueueFilter::Sqs {
                jq_filter: Some(jq),
                ..
            } => Some((name.as_str(), jq.as_str())),
            _ => None,
        })
    }

    /// Out of the whole queue splitting config, get only the kafka topics.
    pub fn kafka(&self) -> impl '_ + Iterator<Item = (&'_ str, &'_ QueueMessageFilter)> {
        self.0.iter().filter_map(|(name, filter)| match filter {
            QueueFilter::Kafka { message_filter, .. } => Some((name.as_str(), message_filter)),
            _ => None,
        })
    }

    fn verify_message_attribute_filter(
        queue_id: &QueueId,
        filter: &QueueMessageFilter,
    ) -> Result<(), QueueSplittingVerificationError> {
        for (name, pattern) in filter {
            Regex::new(pattern).map_err(|error| {
                QueueSplittingVerificationError::InvalidRegex(
                    queue_id.clone(),
                    name.clone(),
                    error.into(),
                )
            })?;
        }
        Ok(())
    }

    fn verify_jq_program(
        queue_id: &str,
        jq_code: &str,
    ) -> Result<(), QueueSplittingVerificationError> {
        jaq_all::data::compile(jq_code)
            .map(|_| ())
            .map_err(
                |reports| QueueSplittingVerificationError::InvalidJqProgram {
                    queue_name: queue_id.to_string(),
                    jq_compile_errors: reports
                        .iter()
                        .map(FileReportsDisp::new)
                        .map(|report_disp| report_disp.to_string())
                        .collect::<Vec<_>>()
                        .join("\n"),
                },
            )
    }

    pub fn verify(
        &self,
        _context: &mut ConfigContext,
    ) -> Result<(), QueueSplittingVerificationError> {
        for (queue_name, filter) in &self.0 {
            match filter {
                QueueFilter::Sqs {
                    message_filter,
                    jq_filter,
                } => {
                    if let Some(filter) = message_filter {
                        Self::verify_message_attribute_filter(queue_name, filter)?;
                    }
                    if let Some(jq_filter) = jq_filter {
                        Self::verify_jq_program(queue_name, jq_filter)?;
                    }
                }
                QueueFilter::Kafka { message_filter } => {
                    Self::verify_message_attribute_filter(queue_name, message_filter)?;
                }
                QueueFilter::Unknown => {
                    return Err(QueueSplittingVerificationError::UnknownQueueType(
                        queue_name.clone(),
                    ));
                }
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

/// Amazon Simple Queue Service and Kafka are supported.
///
/// More queue types might be added in the future.
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, JsonSchema)]
#[serde(tag = "queue_type")]
pub enum QueueFilter {
    #[serde(rename = "SQS")]
    Sqs {
        /// A filter is a mapping between message attribute names and regexes they should match.
        /// The local application will only receive messages that match **all** of the given
        /// patterns. This means, only messages that have **all** of the attributes in the
        /// filter, with values of those attributes matching the respective patterns.
        #[serde(skip_serializing_if = "Option::is_none")]
        message_filter: Option<QueueMessageFilter>,

        /// A jq filter.
        ///
        /// When this is specified, for each SQS message, a JSON will be constructed with this
        /// form:
        ///
        /// ```json
        /// {
        ///   "message_attributes": {
        ///     "attribute1": "value1",
        ///     "attribute2": "value2"
        ///   }
        ///   "message_body": "whatever is in the body, could be a JSON"
        /// }
        /// ```
        ///
        /// The js filter will run with that JSON as an input, and if it outputs `true`, that
        /// message will be considered as matching the filter.
        #[serde(skip_serializing_if = "Option::is_none")]
        jq_filter: Option<String>,
    },

    #[serde(rename = "Kafka")]
    Kafka {
        /// A filter is a mapping between message header names and regexes they should match.
        /// The local application will only receive messages that match **all** of the given
        /// patterns. This means, only messages that have **all** of the headers in the
        /// filter, with values of those headers matching the respective patterns.
        message_filter: QueueMessageFilter,
    },

    /// When a newer client sends a new filter kind to an older operator, that does not yet know
    /// about that filter type, the filter will be deserialized to unknown.
    #[schemars(skip)]
    #[serde(other, skip_serializing)]
    Unknown,
}

impl CollectAnalytics for &SplitQueuesConfig {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        analytics.add("sqs_queue_count", self.sqs_queues().count());
        // The number of SQS queues filtered with message attribute filters.
        analytics.add("sqs_message_attr_filter_queue_count", self.sqs().count());
        // The number of SQS queues filtered with jq filters.
        analytics.add("sqs_jq_filter_count", self.sqs_jq_filters().count());
        analytics.add("kafka_queue_count", self.kafka().count());
    }
}

#[derive(Error, Debug)]
pub enum QueueSplittingVerificationError {
    #[error("{0}: unknown queue type")]
    UnknownQueueType(String),
    #[error("{0}.message_filter.{1}: failed to parse regular expression ({2})")]
    InvalidRegex(
        String,
        String,
        // without `Box`, clippy complains when `ConfigError` is used in `Err`
        Box<fancy_regex::Error>,
    ),
    #[error("Invalid jq program in filter for queue {queue_name}. Errors:\n{jq_compile_errors}")]
    InvalidJqProgram {
        queue_name: String,
        jq_compile_errors: String,
    },
}

#[cfg(test)]
mod test {
    use super::{QueueFilter, SplitQueuesConfig};

    #[test]
    fn deserialize_known_queue_types() {
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

        let value = serde_json::json!({
            "queue_type": "SQS",
            "message_filter": {
                "key": "value",
            },
        });

        let filter = serde_json::from_value::<QueueFilter>(value).unwrap();
        assert_eq!(
            filter,
            QueueFilter::Sqs {
                message_filter: Some([("key".to_string(), "value".to_string())].into()),
                jq_filter: None,
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

    #[test]
    fn deserialize_sqs_jq_filter() {
        let value = serde_json::json!({
            "queue_type": "SQS",
            "jq_filter": "whatever"
        });

        let filter = serde_json::from_value::<QueueFilter>(value).unwrap();
        assert_eq!(
            filter,
            QueueFilter::Sqs {
                jq_filter: Some("whatever".to_string()),
                message_filter: None
            }
        );
    }

    #[test]
    fn deserialize_sqs_with_both_jq_filter_and_attribute_filter() {
        let value = serde_json::json!({
            "queue_type": "SQS",
            "jq_filter": "whatever",
            "message_filter": {
                "who": "me",
            }
        });

        let filter = serde_json::from_value::<QueueFilter>(value).unwrap();
        assert_eq!(
            filter,
            QueueFilter::Sqs {
                jq_filter: Some("whatever".to_string()),
                message_filter: Some([("who".to_string(), "me".to_string())].into()),
            }
        );
    }

    #[test]
    fn jq_verification_valid_programs() {
        SplitQueuesConfig::verify_jq_program("_", ".snow").unwrap();
        SplitQueuesConfig::verify_jq_program("_", "{snow, wind}").unwrap();
        SplitQueuesConfig::verify_jq_program("_", ".[]").unwrap();
        SplitQueuesConfig::verify_jq_program("_", ".[] | select(.snow > 25)").unwrap();
    }

    #[test]
    fn jq_verification_fails_on_invalid_programs() {
        SplitQueuesConfig::verify_jq_program("_", "snow").unwrap_err();
        SplitQueuesConfig::verify_jq_program("_", "").unwrap_err();
        SplitQueuesConfig::verify_jq_program("_", "idk | whatever").unwrap_err();
    }
}
