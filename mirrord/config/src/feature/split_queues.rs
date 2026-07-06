use std::{collections::BTreeMap, fmt};

use fancy_regex::Regex;
use mirrord_analytics::{Analytics, CollectAnalytics};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{
    Deserialize, Serialize,
    de::{MapAccess, SeqAccess, Visitor},
    ser::SerializeMap,
};
use thiserror::Error;

use crate::config::{ConfigContext, FromMirrordConfig, MirrordConfig};

pub type QueueId = String;

/// The queue splitting configuration. Each entry pairs a queue id with a filter that decides which
/// messages from the original queue are delivered to the local application, based on message
/// attributes or headers, and possibly on jq filters (for SQS and other body-aware brokers).
///
/// The queue ids have to match those defined in the `MirrordWorkloadQueueRegistry` for SQS and
/// RabbitMQ or `MirrordKafkaTopicsConsumer` for Kafka.
///
/// Two shapes are accepted. The classic map form keys each filter by its queue id, which means a
/// given id can appear only once:
///
/// ```json
/// {
///   "feature": {
///     "split_queues": {
///       "first-queue": {
///         "queue_type": "SQS",
///         "message_filter": { "wows": "so wows", "coolz": "^very" }
///       },
///       "second-queue": {
///         "queue_type": "Kafka",
///         "message_filter": { "who": "you$" }
///       }
///     }
///   }
/// }
/// ```
///
/// The list form moves the id into each entry, so the same id can be used more than once - for
/// example to split a queue with the same name on two different brokers:
///
/// ```json
/// {
///   "feature": {
///     "split_queues": [
///       {
///         "queue_id": "orders",
///         "queue_type": "SQS",
///         "message_filter": { "region": "^eu" }
///       },
///       {
///         "queue_id": "orders",
///         "queue_type": "Kafka",
///         "message_filter": { "region": "^us" }
///       }
///     ]
///   }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct SplitQueuesConfig(Vec<QueueSplit>);

/// A single queue splitting entry: the queue id together with its filter. Keeping the id next to
/// the filter (instead of using it as a map key) is what lets the same id show up more than once,
/// which a map cannot do.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QueueSplit {
    /// The id of the queue to split. Does not have to be unique across entries.
    pub queue_id: QueueId,

    /// The filter for this queue, tagged by its `queue_type`.
    #[serde(flatten)]
    pub filter: QueueFilter,
}

impl From<(QueueId, QueueFilter)> for QueueSplit {
    fn from((queue_id, filter): (QueueId, QueueFilter)) -> Self {
        Self { queue_id, filter }
    }
}

impl SplitQueuesConfig {
    /// Returns whether this configuration contains any queue at all.
    pub fn is_set(&self) -> bool {
        !self.0.is_empty()
    }

    /// All the queue splitting entries.
    pub fn splits(&self) -> &[QueueSplit] {
        &self.0
    }

    /// Get all the SQS queue ids from the config.
    pub fn sqs_queues(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::Sqs { .. } => Some(split.queue_id.as_str()),
            _ => None,
        })
    }

    /// Out of the whole queue splitting config, get only the sqs message attribute filters.
    pub fn sqs(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::Sqs {
                message_filter: Some(message_filter),
                ..
            } => Some((split.queue_id.as_str(), message_filter)),
            _ => None,
        })
    }

    /// Out of the whole queue splitting config, get only the sqs jq filters.
    pub fn sqs_jq_filters(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::Sqs {
                jq_filter: Some(jq),
                ..
            } => Some((split.queue_id.as_str(), jq.as_str())),
            _ => None,
        })
    }

    /// Out of the whole queue splitting config, get only the kafka topics.
    pub fn kafka(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::Kafka { message_filter, .. } => {
                Some((split.queue_id.as_str(), message_filter))
            }
            _ => None,
        })
    }

    pub fn rmq(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::Rmq { message_filter } => Some((split.queue_id.as_str(), message_filter)),
            _ => None,
        })
    }

    pub fn gcp_pubsub(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::GcpPubSub {
                message_filter: Some(message_filter),
                ..
            } => Some((split.queue_id.as_str(), message_filter)),
            _ => None,
        })
    }

    pub fn gcp_pubsub_jq_filters(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::GcpPubSub {
                jq_filter: Some(jq),
                ..
            } => Some((split.queue_id.as_str(), jq.as_str())),
            _ => None,
        })
    }

    pub fn gcp_pubsub_queues(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::GcpPubSub { .. } => Some(split.queue_id.as_str()),
            _ => None,
        })
    }

    pub fn azure_service_bus(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::AzureServiceBus {
                message_filter: Some(message_filter),
                ..
            } => Some((split.queue_id.as_str(), message_filter)),
            _ => None,
        })
    }

    pub fn azure_service_bus_jq_filters(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::AzureServiceBus {
                jq_filter: Some(jq),
                ..
            } => Some((split.queue_id.as_str(), jq.as_str())),
            _ => None,
        })
    }

    pub fn azure_service_bus_queues(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::AzureServiceBus { .. } => Some(split.queue_id.as_str()),
            _ => None,
        })
    }

    pub fn redis_pubsub(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::RedisPubSub {
                message_filter: Some(message_filter),
                ..
            } => Some((split.queue_id.as_str(), message_filter)),
            _ => None,
        })
    }

    pub fn temporal(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::Temporal {
                message_filter: Some(message_filter),
                ..
            } => Some((split.queue_id.as_str(), message_filter)),
            _ => None,
        })
    }

    pub fn redis_pubsub_jq_filters(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::RedisPubSub {
                jq_filter: Some(jq),
                ..
            } => Some((split.queue_id.as_str(), jq.as_str())),
            _ => None,
        })
    }

    pub fn temporal_jq_filters(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::Temporal {
                jq_filter: Some(jq),
                ..
            } => Some((split.queue_id.as_str(), jq.as_str())),
            _ => None,
        })
    }

    pub fn redis_pubsub_queues(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::RedisPubSub { .. } => Some(split.queue_id.as_str()),
            _ => None,
        })
    }

    pub fn temporal_queues(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::Temporal { .. } => Some(split.queue_id.as_str()),
            _ => None,
        })
    }

    pub fn bullmq(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::BullMq {
                message_filter: Some(message_filter),
                ..
            } => Some((split.queue_id.as_str(), message_filter)),
            _ => None,
        })
    }

    pub fn bullmq_jq_filters(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::BullMq {
                jq_filter: Some(jq),
                ..
            } => Some((split.queue_id.as_str(), jq.as_str())),
            _ => None,
        })
    }

    pub fn bullmq_queues(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::BullMq { .. } => Some(split.queue_id.as_str()),
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
        mirrord_jaq::compile_jq(jq_code).map(|_| ()).map_err(|err| {
            QueueSplittingVerificationError::InvalidJqProgram {
                queue_name: queue_id.to_string(),
                jq_compile_errors: err.to_string(),
            }
        })
    }

    pub fn verify(
        &self,
        _context: &mut ConfigContext,
    ) -> Result<(), QueueSplittingVerificationError> {
        for split in &self.0 {
            let queue_name = &split.queue_id;
            match &split.filter {
                QueueFilter::Sqs {
                    message_filter,
                    jq_filter,
                }
                | QueueFilter::GcpPubSub {
                    message_filter,
                    jq_filter,
                }
                | QueueFilter::AzureServiceBus {
                    message_filter,
                    jq_filter,
                }
                | QueueFilter::RedisPubSub {
                    message_filter,
                    jq_filter,
                }
                | QueueFilter::Temporal {
                    message_filter,
                    jq_filter,
                }
                | QueueFilter::BullMq {
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
                QueueFilter::Kafka { message_filter } | QueueFilter::Rmq { message_filter } => {
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

impl Serialize for SplitQueuesConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // When every id is unique we emit the classic map form. This keeps older readers and the
        // copy-target CRD schema (which expects an object) happy. Only when an id repeats - which a
        // map cannot represent - do we fall back to the list form.
        let mut seen = std::collections::HashSet::with_capacity(self.0.len());
        let has_duplicates = !self.0.iter().all(|split| seen.insert(&split.queue_id));

        if has_duplicates {
            return self.0.serialize(serializer);
        }

        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for split in &self.0 {
            map.serialize_entry(&split.queue_id, &split.filter)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for SplitQueuesConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct SplitQueuesVisitor;

        impl<'de> Visitor<'de> for SplitQueuesVisitor {
            type Value = SplitQueuesConfig;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str(
                    "a map from queue id to its filter, or a list of queue split entries",
                )
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut splits = Vec::with_capacity(map.size_hint().unwrap_or(0));
                while let Some((queue_id, filter)) = map.next_entry::<QueueId, QueueFilter>()? {
                    splits.push(QueueSplit { queue_id, filter });
                }
                Ok(SplitQueuesConfig(splits))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut splits = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(split) = seq.next_element::<QueueSplit>()? {
                    splits.push(split);
                }
                Ok(SplitQueuesConfig(splits))
            }
        }

        deserializer.deserialize_any(SplitQueuesVisitor)
    }
}

impl JsonSchema for SplitQueuesConfig {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SplitQueuesConfig".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let filter = generator.subschema_for::<QueueFilter>().to_value();
        let split = generator.subschema_for::<QueueSplit>().to_value();

        let mut schema = schemars::json_schema!({});
        schema.insert(
            "anyOf".to_owned(),
            serde_json::json!([
                { "type": "object", "additionalProperties": filter },
                { "type": "array", "items": split },
            ]),
        );
        schema
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

/// ### feature.split_queues.{}.message_filter {#feature-split_queues-queue_id-message_filter}
///
/// For each queue, `message_filter` is a mapping between message attribute names and regexes they
/// should match. The local application will only receive messages that match **all** of the given
/// patterns. This means, only messages that have **all** of the attributes in the
/// filter, with values of those attributes matching the respective patterns.
///
/// ### feature.split_queues.{}.queue_type {#feature-split_queues-queue_id-queue_type}
///
/// The type of queue to be split, currently `SQS` and `Kafka` are supported. More queue types might
/// be added in the future.
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, JsonSchema)]
#[serde(tag = "queue_type", deny_unknown_fields)]
pub enum QueueFilter {
    /// ### feature.split_queues.{}.jq_filter {#feature-split_queues-queue_id-jq_filter}
    /// Only supported with `queue_type` of `SQS`, or `GCPPubSub`.
    /// When this field is specified, for each message, the jq filter runs on a JSON
    /// representation of the message. If the jq program outputs `true`, that
    /// message is considered as matching the filter.
    ///
    /// For **SQS**, [an SQS `Message` object](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/APIReference/API_Message.html)
    /// is used.
    ///
    /// For **GCP Pub/Sub**, the JSON representation of [`PubsubMessage`](https://cloud.google.com/pubsub/docs/reference/rest/v1/PubsubMessage)
    /// us used.
    ///
    /// This can be used to filter messages based on their body content, for example.
    ///
    ///
    /// This filter, for example, will tell mirrord to only make available to this local application
    /// messages with a json in the message body, with a `customer_email` field that contains
    /// "metalbear.com": `".Body | fromjson | .customer_email | test(\"metalbear\\\\.com\")"`
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
        /// When this is specified, for each SQS message, the jq filter runs on a JSON
        /// representation of the SQS `Message` object.
        ///
        /// See [SQS `Message` object reference](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/APIReference/API_Message.html).
        ///
        /// If the jq program outputs `true`, that message is considered as matching the filter.
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

    #[serde(rename = "RMQ")]
    Rmq {
        /// A filter is a mapping between message header names and regexes they should match.
        /// The local application will only receive messages that match **all** of the given
        /// patterns. This means, only messages that have **all** of the headers in the
        /// filter, with values of those headers matching the respective patterns.
        message_filter: QueueMessageFilter,
    },

    #[serde(rename = "GCPPubSub")]
    GcpPubSub {
        /// A filter is a mapping between Pub/Sub message attribute names and regexes they
        /// should match. The local application will only receive messages whose attributes
        /// match **all** of the given patterns.
        #[serde(skip_serializing_if = "Option::is_none")]
        message_filter: Option<QueueMessageFilter>,

        /// A jq filter.
        ///
        /// When this is specified, for each Pub/Sub message, the jq filter runs on a JSON
        /// representation of the full
        /// [`PubsubMessage`](https://cloud.google.com/pubsub/docs/reference/rest/v1/PubsubMessage)
        /// object.
        ///
        /// If the jq program outputs `true`, that message is considered as matching the filter.
        #[serde(skip_serializing_if = "Option::is_none")]
        jq_filter: Option<String>,
    },

    #[serde(rename = "RedisPubSub")]
    RedisPubSub {
        /// A filter is a mapping between top-level JSON field names and regexes they
        /// should match. The local application will only receive messages whose JSON
        /// payload contains fields matching **all** of the given patterns.
        #[serde(skip_serializing_if = "Option::is_none")]
        message_filter: Option<QueueMessageFilter>,

        /// A jq filter that runs on the JSON representation of the message payload.
        #[serde(skip_serializing_if = "Option::is_none")]
        jq_filter: Option<String>,
    },

    #[serde(rename = "AzureServiceBus")]
    AzureServiceBus {
        /// A filter is a mapping between Azure Service Bus application property names and
        /// regexes they should match. The local application will only receive messages whose
        /// application properties match **all** of the given patterns.
        #[serde(skip_serializing_if = "Option::is_none")]
        message_filter: Option<QueueMessageFilter>,

        /// A jq filter.
        ///
        /// When this is specified, for each Service Bus message, the jq filter runs on a JSON
        /// representation of the full `ServiceBusMessage` object.
        ///
        /// If the jq program outputs `true`, that message is considered as matching the filter.
        #[serde(skip_serializing_if = "Option::is_none")]
        jq_filter: Option<String>,
    },

    #[serde(rename = "Temporal")]
    Temporal {
        /// Regex filters on Temporal task metadata (`workflow_id`, `workflow_type`,
        /// `activity_type`, or custom search attribute keys). All patterns must match.
        #[serde(skip_serializing_if = "Option::is_none")]
        message_filter: Option<QueueMessageFilter>,

        /// JQ filter on activity task input JSON. Workflow tasks ignore this filter.
        #[serde(skip_serializing_if = "Option::is_none")]
        jq_filter: Option<String>,
    },

    #[serde(rename = "BullMQ")]
    BullMq {
        /// A filter on top-level JSON fields in the job's `data` payload and regexes
        /// they should match. Only jobs whose data contains fields matching **all**
        /// patterns are delivered to the local application.
        #[serde(skip_serializing_if = "Option::is_none")]
        message_filter: Option<QueueMessageFilter>,

        /// A jq filter that runs on the JSON representation of the job's `data` payload.
        #[serde(skip_serializing_if = "Option::is_none")]
        jq_filter: Option<String>,
    },

    // When a newer client sends a new filter kind to an older operator, that does not yet know
    // about that filter type, the filter will be deserialized to unknown.
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
        analytics.add("rmq_queue_count", self.rmq().count());
        analytics.add("gcp_pubsub_queue_count", self.gcp_pubsub_queues().count());
        analytics.add(
            "gcp_pubsub_jq_filter_count",
            self.gcp_pubsub_jq_filters().count(),
        );
        analytics.add(
            "azure_service_bus_queue_count",
            self.azure_service_bus_queues().count(),
        );
        analytics.add(
            "azure_service_bus_jq_filter_count",
            self.azure_service_bus_jq_filters().count(),
        );

        analytics.add(
            "redis_pubsub_queue_count",
            self.redis_pubsub_queues().count(),
        );
        analytics.add(
            "redis_pubsub_jq_filter_count",
            self.redis_pubsub_jq_filters().count(),
        );
        analytics.add("temporal_queue_count", self.temporal_queues().count());
        analytics.add(
            "temporal_jq_filter_count",
            self.temporal_jq_filters().count(),
        );
        analytics.add("bullmq_queue_count", self.bullmq_queues().count());
        analytics.add("bullmq_jq_filter_count", self.bullmq_jq_filters().count());
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
    use super::{QueueFilter, QueueSplit, SplitQueuesConfig};

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
            "queue_type": "RMQ",
            "message_filter": {
                "key": "value",
            },
        });

        let filter = serde_json::from_value::<QueueFilter>(value).unwrap();
        assert_eq!(
            filter,
            QueueFilter::Rmq {
                message_filter: [("key".to_string(), "value".to_string())].into(),
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
    fn deserialize_legacy_map_form() {
        let value = serde_json::json!({
            "first": { "queue_type": "SQS", "message_filter": { "k": "v" } },
            "second": { "queue_type": "Kafka", "message_filter": { "who": "you$" } },
        });

        let config = serde_json::from_value::<SplitQueuesConfig>(value).unwrap();
        assert_eq!(config.sqs_queues().collect::<Vec<_>>(), ["first"]);
        assert_eq!(
            config.kafka().map(|(id, _)| id).collect::<Vec<_>>(),
            ["second"]
        );
    }

    #[test]
    fn deserialize_list_form() {
        let value = serde_json::json!([
            { "queue_id": "first", "queue_type": "SQS", "message_filter": { "k": "v" } },
            { "queue_id": "second", "queue_type": "Kafka", "message_filter": { "who": "you$" } },
        ]);

        let config = serde_json::from_value::<SplitQueuesConfig>(value).unwrap();
        assert_eq!(config.splits().len(), 2);
        assert_eq!(config.sqs_queues().collect::<Vec<_>>(), ["first"]);
    }

    /// The whole point of the list form: the same queue id used for two different brokers.
    #[test]
    fn deserialize_list_form_duplicate_id_across_brokers() {
        let value = serde_json::json!([
            { "queue_id": "orders", "queue_type": "SQS", "message_filter": { "region": "^eu" } },
            { "queue_id": "orders", "queue_type": "Kafka", "message_filter": { "region": "^us" } },
        ]);

        let config = serde_json::from_value::<SplitQueuesConfig>(value).unwrap();
        assert_eq!(
            config.sqs().map(|(id, _)| id).collect::<Vec<_>>(),
            ["orders"]
        );
        assert_eq!(
            config.kafka().map(|(id, _)| id).collect::<Vec<_>>(),
            ["orders"]
        );
    }

    /// Unique ids round-trip through the map form; duplicate ids round-trip through the list form.
    /// Both must deserialize back to the same config.
    #[test]
    fn serialize_round_trip() {
        let unique = SplitQueuesConfig(vec![
            QueueSplit {
                queue_id: "first".to_owned(),
                filter: QueueFilter::Sqs {
                    message_filter: Some([("k".to_owned(), "v".to_owned())].into()),
                    jq_filter: None,
                },
            },
            QueueSplit {
                queue_id: "second".to_owned(),
                filter: QueueFilter::Kafka {
                    message_filter: [("who".to_owned(), "you$".to_owned())].into(),
                },
            },
        ]);
        let json = serde_json::to_value(&unique).unwrap();
        assert!(json.is_object(), "unique ids should serialize as a map");
        assert_eq!(
            serde_json::from_value::<SplitQueuesConfig>(json).unwrap(),
            unique
        );

        let duplicate = SplitQueuesConfig(vec![
            QueueSplit {
                queue_id: "orders".to_owned(),
                filter: QueueFilter::Sqs {
                    message_filter: Some([("region".to_owned(), "^eu".to_owned())].into()),
                    jq_filter: None,
                },
            },
            QueueSplit {
                queue_id: "orders".to_owned(),
                filter: QueueFilter::Kafka {
                    message_filter: [("region".to_owned(), "^us".to_owned())].into(),
                },
            },
        ]);
        let json = serde_json::to_value(&duplicate).unwrap();
        assert!(json.is_array(), "duplicate ids should serialize as a list");
        assert_eq!(
            serde_json::from_value::<SplitQueuesConfig>(json).unwrap(),
            duplicate
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
