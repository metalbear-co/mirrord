use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fmt,
    ops::Not,
};

use fancy_regex::Regex;
use mirrord_analytics::{Analytics, CollectAnalytics};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{
    Deserialize, Serialize,
    de::{MapAccess, SeqAccess, Visitor},
    ser::{SerializeMap, SerializeSeq},
};
use thiserror::Error;

pub use super::queue_metadata_filter::{
    QueueInnerFilter, QueueMetadataFilterConfig, QueueMetadataFilterVerificationError, QueueType,
};
use crate::config::{ConfigContext, FromMirrordConfig, MirrordConfig};

pub type QueueId = String;

/// Queue filters. Each queue filter defines which messages from the original queue will be made
/// available to the local application, based on message attributes or headers, and possibly on jq
/// filters (for SQS).
///
/// Prefer the **list form with `filter`** (HTTP-style metadata matching). The **map form with
/// `message_filter`** is deprecated but still accepted.
///
/// The queue-ids have to match those defined in the `MirrordWorkloadQueueRegistry` for SQS and
/// RabbitMQ or `MirrordKafkaTopicsConsumer` for Kafka. The special id `*` matches every queue of
/// the given type.
///
/// There are two shapes. **Prefer the list form with `filter`.** The map form with
/// `message_filter` is deprecated.
///
/// List form (recommended):
///
/// ```json
/// {
///   "feature": {
///     "split_queues": [
///       {
///         "queue_id": "orders",
///         "queue_type": "SQS",
///         "filter": {
///           "metadata": "^baggage: .*mirrord-session={{ key }}.*"
///         }
///       }
///     ]
///   }
/// }
/// ```
///
/// Use `*` as `queue_id` when the same filter should apply to every queue of that type. When the
/// same `queue_id` and `queue_type` appear more than once, a message matches if **any** entry
/// matches.
///
/// Deprecated map form (unique queue ids only):
///
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
///         "jq_filter": ".Body | fromjson | .customer_email | test(\"metalbear\\\\.com\")"
///       },
///       "third-queue": {
///         "queue_type": "Kafka",
///         "message_filter": {
///           "who": "you$"
///         }
///       }
///     }
///   }
/// }
/// ```
///
/// Deprecated: use list entries with `filter` instead of `message_filter`. A repeated queue id
/// with `message_filter` is rejected at verify time.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct SplitQueuesConfig(Vec<SplitQueueEntry>);

/// One queue split entry in the canonical internal representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitQueueEntry {
    pub queue_id: QueueId,
    pub body: QueueEntryBody,
}

/// Either the legacy per-broker filter (`message_filter` map) or the new HTTP-style metadata
/// filter block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueueEntryBody {
    Legacy(QueueFilter),
    Metadata {
        queue_type: QueueType,
        filter: QueueMetadataFilterConfig,
    },
}

impl QueueEntryBody {
    pub fn uses_metadata_filter(&self) -> bool {
        matches!(self, Self::Metadata { .. })
    }

    pub fn queue_type(&self) -> Option<QueueType> {
        match self {
            Self::Legacy(filter) => filter.queue_type(),
            Self::Metadata { queue_type, .. } => Some(*queue_type),
        }
    }
}

/// Legacy list/map entry: `queue_id` plus a flattened [`QueueFilter`].
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
struct SplitQueueLegacyEntry {
    queue_id: QueueId,
    #[serde(flatten)]
    filter: QueueFilter,
}

/// New list entry: explicit `queue_type` and HTTP-style `filter` block.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
struct SplitQueueMetadataEntry {
    queue_id: QueueId,
    queue_type: QueueType,
    filter: QueueMetadataFilterConfig,
}

#[derive(Serialize)]
#[serde(untagged)]
enum SplitQueueEntryRef<'a> {
    Legacy {
        queue_id: &'a QueueId,
        #[serde(flatten)]
        filter: &'a QueueFilter,
    },
    Metadata {
        queue_id: &'a QueueId,
        queue_type: QueueType,
        filter: &'a QueueMetadataFilterConfig,
    },
}

impl SplitQueuesConfig {
    fn needs_list_form(&self) -> bool {
        let mut seen = BTreeSet::new();
        self.0
            .iter()
            .any(|entry| entry.body.uses_metadata_filter() || !seen.insert(&entry.queue_id))
    }
}

impl Serialize for SplitQueuesConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.needs_list_form() {
            let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
            for entry in &self.0 {
                let serialized = match &entry.body {
                    QueueEntryBody::Legacy(filter) => SplitQueueEntryRef::Legacy {
                        queue_id: &entry.queue_id,
                        filter,
                    },
                    QueueEntryBody::Metadata { queue_type, filter } => {
                        SplitQueueEntryRef::Metadata {
                            queue_id: &entry.queue_id,
                            queue_type: *queue_type,
                            filter,
                        }
                    }
                };
                seq.serialize_element(&serialized)?;
            }
            seq.end()
        } else {
            let mut map = serializer.serialize_map(Some(self.0.len()))?;
            for entry in &self.0 {
                let QueueEntryBody::Legacy(filter) = &entry.body else {
                    continue;
                };
                map.serialize_entry(&entry.queue_id, filter)?;
            }
            map.end()
        }
    }
}

impl<'de> Deserialize<'de> for SplitQueuesConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum SplitQueueEntryDe {
            Legacy(SplitQueueLegacyEntry),
            Metadata(SplitQueueMetadataEntry),
        }

        struct ConfigVisitor;

        impl<'de> Visitor<'de> for ConfigVisitor {
            type Value = SplitQueuesConfig;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter
                    .write_str("a map of queue ids to filters, or a list of queue split entries")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut entries = Vec::new();
                while let Some((queue_id, filter)) = map.next_entry::<QueueId, QueueFilter>()? {
                    entries.push(SplitQueueEntry {
                        queue_id,
                        body: QueueEntryBody::Legacy(filter),
                    });
                }
                Ok(SplitQueuesConfig(entries))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut entries = Vec::new();
                while let Some(entry) = seq.next_element::<SplitQueueEntryDe>()? {
                    entries.push(match entry {
                        SplitQueueEntryDe::Legacy(entry) => SplitQueueEntry {
                            queue_id: entry.queue_id,
                            body: QueueEntryBody::Legacy(entry.filter),
                        },
                        SplitQueueEntryDe::Metadata(entry) => SplitQueueEntry {
                            queue_id: entry.queue_id,
                            body: QueueEntryBody::Metadata {
                                queue_type: entry.queue_type,
                                filter: entry.filter,
                            },
                        },
                    });
                }
                Ok(SplitQueuesConfig(entries))
            }
        }

        deserializer.deserialize_any(ConfigVisitor)
    }
}

impl JsonSchema for SplitQueuesConfig {
    fn schema_name() -> Cow<'static, str> {
        "SplitQueuesConfig".into()
    }

    fn json_schema(schema_gen: &mut SchemaGenerator) -> Schema {
        let any_of = vec![
            schema_gen
                .subschema_for::<BTreeMap<QueueId, QueueFilter>>()
                .to_value(),
            schema_gen
                .subschema_for::<Vec<SplitQueueLegacyEntry>>()
                .to_value(),
            schema_gen
                .subschema_for::<Vec<SplitQueueMetadataEntry>>()
                .to_value(),
        ];

        let mut schema = schemars::json_schema!({});
        schema.insert("anyOf".to_owned(), serde_json::Value::Array(any_of));
        schema
    }
}

impl SplitQueuesConfig {
    /// Returns whether this configuration contains any queue at all.
    pub fn is_set(&self) -> bool {
        !self.0.is_empty()
    }

    /// Get all the SQS queue ids from the config.
    pub fn sqs_queues(&self) -> impl Iterator<Item = &str> {
        self.0
            .iter()
            .filter_map(|entry| match entry.body.queue_type()? {
                QueueType::Sqs => Some(entry.queue_id.as_str()),
                _ => None,
            })
    }

    /// Out of the whole queue splitting config, get only the sqs legacy message attribute filters.
    pub fn sqs(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|entry| match &entry.body {
            QueueEntryBody::Legacy(QueueFilter::Sqs {
                message_filter: Some(message_filter),
                ..
            }) => Some((entry.queue_id.as_str(), message_filter)),
            _ => None,
        })
    }

    /// HTTP-style metadata filters for SQS queues.
    pub fn sqs_metadata(&self) -> impl Iterator<Item = (&str, &QueueMetadataFilterConfig)> {
        self.metadata_for_type(QueueType::Sqs)
    }

    /// Out of the whole queue splitting config, get only the sqs jq filters (legacy form).
    pub fn sqs_jq_filters(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().filter_map(|entry| match &entry.body {
            QueueEntryBody::Legacy(QueueFilter::Sqs {
                jq_filter: Some(jq),
                ..
            }) => Some((entry.queue_id.as_str(), jq.as_str())),
            QueueEntryBody::Metadata {
                queue_type: QueueType::Sqs,
                filter,
            } => filter.jq.as_deref().map(|jq| (entry.queue_id.as_str(), jq)),
            _ => None,
        })
    }

    /// Out of the whole queue splitting config, get only the kafka topics (legacy filters).
    pub fn kafka(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|entry| match &entry.body {
            QueueEntryBody::Legacy(QueueFilter::Kafka { message_filter, .. }) => {
                Some((entry.queue_id.as_str(), message_filter))
            }
            _ => None,
        })
    }

    pub fn kafka_metadata(&self) -> impl Iterator<Item = (&str, &QueueMetadataFilterConfig)> {
        self.metadata_for_type(QueueType::Kafka)
    }

    pub fn rmq(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|entry| match &entry.body {
            QueueEntryBody::Legacy(QueueFilter::Rmq { message_filter }) => {
                Some((entry.queue_id.as_str(), message_filter))
            }
            _ => None,
        })
    }

    pub fn rmq_metadata(&self) -> impl Iterator<Item = (&str, &QueueMetadataFilterConfig)> {
        self.metadata_for_type(QueueType::Rmq)
    }

    pub fn gcp_pubsub(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|entry| match &entry.body {
            QueueEntryBody::Legacy(QueueFilter::GcpPubSub {
                message_filter: Some(message_filter),
                ..
            }) => Some((entry.queue_id.as_str(), message_filter)),
            _ => None,
        })
    }

    pub fn gcp_pubsub_metadata(&self) -> impl Iterator<Item = (&str, &QueueMetadataFilterConfig)> {
        self.metadata_for_type(QueueType::GcpPubSub)
    }

    pub fn gcp_pubsub_jq_filters(&self) -> impl Iterator<Item = (&str, &str)> {
        self.jq_for_type(QueueType::GcpPubSub)
    }

    pub fn gcp_pubsub_queues(&self) -> impl Iterator<Item = &str> {
        self.queue_ids_for_type(QueueType::GcpPubSub)
    }

    pub fn azure_service_bus(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|entry| match &entry.body {
            QueueEntryBody::Legacy(QueueFilter::AzureServiceBus {
                message_filter: Some(message_filter),
                ..
            }) => Some((entry.queue_id.as_str(), message_filter)),
            _ => None,
        })
    }

    pub fn azure_service_bus_metadata(
        &self,
    ) -> impl Iterator<Item = (&str, &QueueMetadataFilterConfig)> {
        self.metadata_for_type(QueueType::AzureServiceBus)
    }

    pub fn azure_service_bus_jq_filters(&self) -> impl Iterator<Item = (&str, &str)> {
        self.jq_for_type(QueueType::AzureServiceBus)
    }

    pub fn azure_service_bus_queues(&self) -> impl Iterator<Item = &str> {
        self.queue_ids_for_type(QueueType::AzureServiceBus)
    }

    pub fn redis_pubsub(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|entry| match &entry.body {
            QueueEntryBody::Legacy(QueueFilter::RedisPubSub {
                message_filter: Some(message_filter),
                ..
            }) => Some((entry.queue_id.as_str(), message_filter)),
            _ => None,
        })
    }

    pub fn redis_pubsub_metadata(
        &self,
    ) -> impl Iterator<Item = (&str, &QueueMetadataFilterConfig)> {
        self.metadata_for_type(QueueType::RedisPubSub)
    }

    pub fn temporal(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|entry| match &entry.body {
            QueueEntryBody::Legacy(QueueFilter::Temporal {
                message_filter: Some(message_filter),
                ..
            }) => Some((entry.queue_id.as_str(), message_filter)),
            _ => None,
        })
    }

    pub fn temporal_metadata(&self) -> impl Iterator<Item = (&str, &QueueMetadataFilterConfig)> {
        self.metadata_for_type(QueueType::Temporal)
    }

    pub fn redis_pubsub_jq_filters(&self) -> impl Iterator<Item = (&str, &str)> {
        self.jq_for_type(QueueType::RedisPubSub)
    }

    pub fn temporal_jq_filters(&self) -> impl Iterator<Item = (&str, &str)> {
        self.jq_for_type(QueueType::Temporal)
    }

    pub fn redis_pubsub_queues(&self) -> impl Iterator<Item = &str> {
        self.queue_ids_for_type(QueueType::RedisPubSub)
    }

    pub fn temporal_queues(&self) -> impl Iterator<Item = &str> {
        self.queue_ids_for_type(QueueType::Temporal)
    }

    /// All list-form metadata filters, with their queue type.
    pub fn metadata(&self) -> impl Iterator<Item = (QueueType, &str, &QueueMetadataFilterConfig)> {
        self.0.iter().filter_map(|entry| match &entry.body {
            QueueEntryBody::Metadata { queue_type, filter } => {
                Some((*queue_type, entry.queue_id.as_str(), filter))
            }
            _ => None,
        })
    }

    fn metadata_for_type(
        &self,
        queue_type: QueueType,
    ) -> impl Iterator<Item = (&str, &QueueMetadataFilterConfig)> {
        self.0.iter().filter_map(move |entry| match &entry.body {
            QueueEntryBody::Metadata {
                queue_type: entry_type,
                filter,
            } if *entry_type == queue_type => Some((entry.queue_id.as_str(), filter)),
            _ => None,
        })
    }

    fn jq_for_type(&self, queue_type: QueueType) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().filter_map(move |entry| match &entry.body {
            QueueEntryBody::Legacy(filter) if filter.queue_type() == Some(queue_type) => filter
                .legacy_jq_filter()
                .map(|jq| (entry.queue_id.as_str(), jq)),
            QueueEntryBody::Metadata {
                queue_type: entry_type,
                filter,
            } if *entry_type == queue_type => {
                filter.jq.as_deref().map(|jq| (entry.queue_id.as_str(), jq))
            }
            _ => None,
        })
    }

    fn queue_ids_for_type(&self, queue_type: QueueType) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(move |entry| {
            (entry.body.queue_type()? == queue_type).then_some(entry.queue_id.as_str())
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
        context: &mut ConfigContext,
    ) -> Result<(), QueueSplittingVerificationError> {
        let mut legacy_queue_ids = BTreeSet::new();
        for entry in &self.0 {
            if matches!(entry.body, QueueEntryBody::Legacy(_))
                && !legacy_queue_ids.insert(&entry.queue_id)
            {
                return Err(QueueSplittingVerificationError::DuplicateLegacyQueueId(
                    entry.queue_id.clone(),
                ));
            }
        }

        if self
            .0
            .iter()
            .any(|entry| entry.body.uses_metadata_filter().not())
        {
            context.add_warning(
                "`feature.split_queues` with `message_filter` or legacy map `jq_filter` is \
                deprecated. Use a list of entries with `filter` instead - see \
                https://metalbear.com/mirrord/docs/sharing-the-cluster/queue-splitting"
                    .to_owned(),
            );
        }

        for entry in &self.0 {
            match &entry.body {
                QueueEntryBody::Legacy(filter) => match filter {
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
                    } => {
                        if let Some(filter) = message_filter {
                            Self::verify_message_attribute_filter(&entry.queue_id, filter)?;
                        }
                        if let Some(jq_filter) = jq_filter {
                            Self::verify_jq_program(&entry.queue_id, jq_filter)?;
                        }
                    }
                    QueueFilter::Kafka { message_filter } | QueueFilter::Rmq { message_filter } => {
                        Self::verify_message_attribute_filter(&entry.queue_id, message_filter)?;
                    }
                    QueueFilter::Unknown => {
                        return Err(QueueSplittingVerificationError::UnknownQueueType(
                            entry.queue_id.clone(),
                        ));
                    }
                },
                QueueEntryBody::Metadata { filter, .. } => filter
                    .verify(&entry.queue_id)
                    .map_err(QueueSplittingVerificationError::MetadataFilter)?,
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

/// ### feature.split_queues.{}.message_filter {#feature-split_queues-queue_id-message_filter}
///
/// **Deprecated.** Use [`filter`](#feature-split_queues-queue_id-filter) on a list entry instead.
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
    ///
    /// **Deprecated.** Prefer list entries with `filter` instead of map entries with
    /// `message_filter` / `jq_filter`.
    #[serde(rename = "SQS")]
    Sqs {
        /// **Deprecated.** Prefer `filter.metadata` or `filter.all_of` on a list entry.
        #[serde(skip_serializing_if = "Option::is_none")]
        message_filter: Option<QueueMessageFilter>,

        /// **Deprecated.** Prefer `filter.jq` on a list entry.
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

    // When a newer client sends a new filter kind to an older operator, that does not yet know
    // about that filter type, the filter will be deserialized to unknown.
    #[schemars(skip)]
    #[serde(other, skip_serializing)]
    Unknown,
}

impl QueueFilter {
    pub fn queue_type(&self) -> Option<QueueType> {
        match self {
            Self::Sqs { .. } => Some(QueueType::Sqs),
            Self::Kafka { .. } => Some(QueueType::Kafka),
            Self::Rmq { .. } => Some(QueueType::Rmq),
            Self::GcpPubSub { .. } => Some(QueueType::GcpPubSub),
            Self::RedisPubSub { .. } => Some(QueueType::RedisPubSub),
            Self::AzureServiceBus { .. } => Some(QueueType::AzureServiceBus),
            Self::Temporal { .. } => Some(QueueType::Temporal),
            Self::Unknown => None,
        }
    }

    pub fn legacy_jq_filter(&self) -> Option<&str> {
        match self {
            Self::Sqs { jq_filter, .. }
            | Self::GcpPubSub { jq_filter, .. }
            | Self::AzureServiceBus { jq_filter, .. }
            | Self::RedisPubSub { jq_filter, .. }
            | Self::Temporal { jq_filter, .. } => jq_filter.as_deref(),
            Self::Kafka { .. } | Self::Rmq { .. } | Self::Unknown => None,
        }
    }
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
    #[error(transparent)]
    MetadataFilter(#[from] QueueMetadataFilterVerificationError),
    #[error(
        "queue id {0} is used more than once with message_filter; use filter on a list entry instead"
    )]
    DuplicateLegacyQueueId(String),
}

#[cfg(test)]
mod test {
    use super::{
        QueueEntryBody, QueueFilter, QueueMetadataFilterConfig, QueueSplittingVerificationError,
        QueueType, SplitQueueEntry, SplitQueuesConfig,
    };

    fn legacy_entry(queue_id: &str, filter: QueueFilter) -> SplitQueueEntry {
        SplitQueueEntry {
            queue_id: queue_id.to_owned(),
            body: QueueEntryBody::Legacy(filter),
        }
    }

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
    fn deserialize_map_form() {
        let value = serde_json::json!({
            "orders": { "queue_type": "SQS", "message_filter": { "k": "v" } },
            "events": { "queue_type": "Kafka", "message_filter": { "h": "x" } },
        });

        let config = serde_json::from_value::<SplitQueuesConfig>(value).unwrap();
        assert_eq!(config.sqs_queues().collect::<Vec<_>>(), vec!["orders"]);
        assert_eq!(config.kafka().count(), 1);
    }

    #[test]
    fn deserialize_list_form() {
        let value = serde_json::json!([
            { "queue_id": "orders", "queue_type": "SQS", "message_filter": { "k": "v" } },
            { "queue_id": "events", "queue_type": "Kafka", "message_filter": { "h": "x" } },
        ]);

        let config = serde_json::from_value::<SplitQueuesConfig>(value).unwrap();
        assert_eq!(config.sqs_queues().collect::<Vec<_>>(), vec!["orders"]);
        assert_eq!(config.kafka().count(), 1);
    }

    /// The map form cannot express this at all, so it only works with the list form.
    #[test]
    fn duplicate_legacy_queue_id_fails_verify() {
        let value = serde_json::json!([
            { "queue_id": "*", "queue_type": "SQS", "message_filter": { "s": "^x$" } },
            { "queue_id": "*", "queue_type": "RMQ", "message_filter": { "s": "^x$" } },
        ]);

        let config = serde_json::from_value::<SplitQueuesConfig>(value).unwrap();
        assert!(matches!(
            config.verify(&mut crate::config::ConfigContext::default()),
            Err(QueueSplittingVerificationError::DuplicateLegacyQueueId(id)) if id == "*"
        ));
    }

    #[test]
    fn duplicate_legacy_same_queue_type_fails_verify() {
        let value = serde_json::json!([
            { "queue_id": "*", "queue_type": "SQS", "message_filter": { "a": "^1$" } },
            { "queue_id": "*", "queue_type": "SQS", "message_filter": { "b": "^2$" } },
        ]);

        let config = serde_json::from_value::<SplitQueuesConfig>(value).unwrap();
        assert!(
            config
                .verify(&mut crate::config::ConfigContext::default())
                .is_err()
        );
    }

    #[test]
    fn serialize_unique_legacy_uses_map_form() {
        let config = SplitQueuesConfig(vec![
            legacy_entry(
                "orders",
                QueueFilter::Sqs {
                    message_filter: Some([("k".to_owned(), "v".to_owned())].into()),
                    jq_filter: None,
                },
            ),
            legacy_entry(
                "events",
                QueueFilter::Kafka {
                    message_filter: [("h".to_owned(), "x".to_owned())].into(),
                },
            ),
        ]);

        let json = serde_json::to_value(&config).unwrap();
        assert!(
            json.is_object(),
            "unique ids should serialize as a map: {json}"
        );
        let mut round_trip = serde_json::from_value::<SplitQueuesConfig>(json).unwrap().0;
        let mut expected = config.0.clone();
        round_trip.sort_by(|a, b| a.queue_id.cmp(&b.queue_id));
        expected.sort_by(|a, b| a.queue_id.cmp(&b.queue_id));
        assert_eq!(expected, round_trip);
    }

    #[test]
    fn serialize_metadata_duplicates_uses_list_form() {
        let config = SplitQueuesConfig(vec![
            SplitQueueEntry {
                queue_id: "*".to_owned(),
                body: QueueEntryBody::Metadata {
                    queue_type: QueueType::Sqs,
                    filter: QueueMetadataFilterConfig {
                        metadata: Some(".*a.*".to_owned()),
                        ..Default::default()
                    },
                },
            },
            SplitQueueEntry {
                queue_id: "*".to_owned(),
                body: QueueEntryBody::Metadata {
                    queue_type: QueueType::Sqs,
                    filter: QueueMetadataFilterConfig {
                        metadata: Some(".*b.*".to_owned()),
                        ..Default::default()
                    },
                },
            },
        ]);

        let json = serde_json::to_value(&config).unwrap();
        assert!(
            json.is_array(),
            "metadata duplicates serialize as a list: {json}"
        );
    }

    #[test]
    fn deserialize_metadata_filter_list_form() {
        let value = serde_json::json!([
            {
                "queue_id": "*",
                "queue_type": "RMQ",
                "filter": { "metadata": ".*mirrord-session=.*" }
            },
            {
                "queue_id": "*",
                "queue_type": "Temporal",
                "filter": {
                    "any_of": [
                        { "metadata": "^baggage: .*mirrord-session=.*$" },
                        { "metadata": "^tracestate: .*mirrord-session=.*$" }
                    ]
                }
            }
        ]);

        let config = serde_json::from_value::<SplitQueuesConfig>(value).unwrap();
        assert_eq!(config.rmq_metadata().count(), 1);
        assert_eq!(config.temporal_metadata().count(), 1);
    }

    #[test]
    fn serialize_metadata_filter_uses_list_form() {
        let config = SplitQueuesConfig(vec![SplitQueueEntry {
            queue_id: "orders".to_owned(),
            body: QueueEntryBody::Metadata {
                queue_type: QueueType::Rmq,
                filter: QueueMetadataFilterConfig {
                    metadata: Some(".*session=.*".to_owned()),
                    ..Default::default()
                },
            },
        }]);

        let json = serde_json::to_value(&config).unwrap();
        assert!(
            json.is_array(),
            "metadata filters serialize as list: {json}"
        );
        let round_trip = serde_json::from_value::<SplitQueuesConfig>(json).unwrap();
        assert_eq!(config, round_trip);
    }

    #[test]
    fn rejects_invalid_metadata_filter_combo() {
        let value = serde_json::json!([
            {
                "queue_id": "q",
                "queue_type": "SQS",
                "filter": {
                    "metadata": ".*",
                    "any_of": [{ "metadata": ".*" }]
                }
            }
        ]);

        let config = serde_json::from_value::<SplitQueuesConfig>(value).unwrap();
        assert!(
            config
                .verify(&mut crate::config::ConfigContext::default())
                .is_err()
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
