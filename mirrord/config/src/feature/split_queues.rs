use std::{
    collections::BTreeMap,
    fmt,
    io::{Read, Write},
    ops::Not,
    path::{Path, PathBuf},
};

use base64::{Engine, prelude::BASE64_STANDARD};
use fancy_regex::Regex;
use mirrord_analytics::{Analytics, CollectAnalytics};
use prost::Message;
use prost_reflect::DescriptorPool;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{
    Deserialize, Serialize,
    de::{MapAccess, SeqAccess, Visitor},
    ser::SerializeMap,
};
use strum_macros::{EnumDiscriminants, EnumIter};
use thiserror::Error;

use crate::{
    config::{ConfigContext, FromMirrordConfig, MirrordConfig},
    env_key::EnvKey,
};

pub type QueueId = String;

/// ### feature.split_queues.{}.queue_mode {#feature-split_queues-queue_id-queue_mode}
///
/// Controls what happens to a message that matches this session's filter.
///
/// - `steal` (default): the matched message is delivered only to this session, so the deployed
///   application never sees it.
/// - `mirror`: the matched message is delivered to this session **and** still delivered to the
///   deployed application, so both process a copy.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, PartialEq, Default, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum QueueMode {
    /// Take matched messages away from the deployed application (current behavior).
    #[default]
    Steal,
    /// Deliver matched messages to this session while the deployed application still gets them.
    Mirror,
}

impl QueueMode {
    /// The default mode does not need to be sent over the wire or persisted, so this gates
    /// `skip_serializing_if`.
    pub fn is_steal(&self) -> bool {
        matches!(self, QueueMode::Steal)
    }
}

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

    /// Whether matched messages are stolen from the deployed application or mirrored to it.
    #[serde(default, skip_serializing_if = "QueueMode::is_steal")]
    pub queue_mode: QueueMode,

    /// ### feature.split_queues.{}.debug {#feature-split_queues-queue_id-debug}
    ///
    /// When `true`, the operator streams how this queue's messages evaluate against your
    /// filter - the exact JSON the jq program runs against, the verdict, and any jq error -
    /// as session events. Read them with `mirrord subscribe` (requires a session `key` in
    /// the config) while the session runs, to find out why a filter is not matching, or to
    /// capture real message shapes for writing one. Off by default; the debug stream
    /// includes message contents.
    #[serde(default, skip_serializing_if = "Not::not")]
    pub debug: bool,

    /// The filter for this queue, tagged by its `queue_type`.
    #[serde(flatten)]
    pub filter: QueueFilter,
}

impl From<(QueueId, QueueFilter)> for QueueSplit {
    fn from((queue_id, filter): (QueueId, QueueFilter)) -> Self {
        Self {
            queue_id,
            queue_mode: QueueMode::default(),
            debug: false,
            filter,
        }
    }
}

impl SplitQueuesConfig {
    pub fn from_splits(splits: impl IntoIterator<Item = QueueSplit>) -> Self {
        Self(splits.into_iter().collect())
    }

    /// Writes a [`SplitQueuesConfig`] with every jq-capable queue type where `queue_id = *`.
    ///
    /// Mainly for `mirrord up`, so the user doesn't have to configure any queue splitting stuff, it
    /// gets handled by the operator instead.
    ///
    /// Queue types the operator has disabled are dropped on its side instead of failing the
    /// session, so listing all of them here is safe.
    pub fn all_wildcard(key: &EnvKey) -> Self {
        let sqs_jq_filter = Self::session_key_string_value_jq(".MessageAttributes", key);
        let kafka_jq_filter = Self::session_key_string_value_jq(".headers", key);
        let gcp_pubsub_jq_filter = Self::session_key_string_value_jq(".attributes", key);
        let azure_service_bus_jq_filter =
            Self::session_key_string_value_jq(".application_properties", key);
        let temporal_jq_filter = Self::session_key_string_value_jq(".header", key);
        let payload_jq_filter = Self::session_key_string_value_jq(".", key);

        Self::from_splits([
            QueueSplit {
                queue_id: "*".to_owned(),
                debug: false,
                filter: QueueFilter::Sqs {
                    message_filter: None,
                    jq_filter: Some(sqs_jq_filter),
                },
                queue_mode: QueueMode::default(),
            },
            QueueSplit {
                queue_id: "*".to_owned(),
                debug: false,
                filter: QueueFilter::Kafka {
                    message_filter: None,
                    jq_filter: Some(kafka_jq_filter),
                    payload_protobuf: None,
                },
                queue_mode: QueueMode::default(),
            },
            QueueSplit {
                queue_id: "*".to_owned(),
                debug: false,
                filter: QueueFilter::GcpPubSub {
                    message_filter: None,
                    jq_filter: Some(gcp_pubsub_jq_filter),
                },
                queue_mode: QueueMode::default(),
            },
            QueueSplit {
                queue_id: "*".to_owned(),
                debug: false,
                filter: QueueFilter::AzureServiceBus {
                    message_filter: None,
                    jq_filter: Some(azure_service_bus_jq_filter),
                },
                queue_mode: QueueMode::default(),
            },
            QueueSplit {
                queue_id: "*".to_owned(),
                debug: false,
                filter: QueueFilter::RedisPubSub {
                    message_filter: None,
                    jq_filter: Some(payload_jq_filter.clone()),
                },
                queue_mode: QueueMode::default(),
            },
            QueueSplit {
                queue_id: "*".to_owned(),
                debug: false,
                filter: QueueFilter::Temporal {
                    message_filter: None,
                    jq_filter: Some(temporal_jq_filter),
                },
                queue_mode: QueueMode::default(),
            },
            QueueSplit {
                queue_id: "*".to_owned(),
                debug: false,
                filter: QueueFilter::BullMq {
                    message_filter: None,
                    jq_filter: Some(payload_jq_filter),
                },
                queue_mode: QueueMode::default(),
            },
        ])
    }

    /// Builds the automatic queue-splitting jq filter used by `mirrord up`.
    ///
    /// The selector points at the broker-specific metadata object that jq sees (for example SQS
    /// message attributes or GCP Pub/Sub attributes). The generated program matches when any
    /// string value under that object contains the propagated `mirrord-session=<key>` marker.
    fn session_key_string_value_jq(selector: &str, key: &EnvKey) -> String {
        let session_marker = serde_json::to_string(&format!("mirrord-session={}", key.as_str()))
            .expect("serializing a string as a JSON string cannot fail");
        format!(
            r#"({selector} // {{}}) | [.. | select(type == "string" and contains({session_marker}))] | length > 0"#
        )
    }

    pub fn is_all_wildcard(&self, key: &EnvKey) -> bool {
        self == &Self::all_wildcard(key)
    }

    /// Returns whether this configuration contains any queue at all.
    pub fn is_set(&self) -> bool {
        !self.0.is_empty()
    }

    /// All the queue splitting entries.
    pub fn splits(&self) -> &[QueueSplit] {
        &self.0
    }

    /// Queue ids whose mode is not the default `steal`, paired with their mode. Only these need to
    /// be sent to the operator; everything else is `steal`.
    pub fn queue_modes(&self) -> impl Iterator<Item = (&str, QueueMode)> {
        self.0.iter().filter_map(|split| {
            (!split.queue_mode.is_steal()).then_some((split.queue_id.as_str(), split.queue_mode))
        })
    }

    /// Queue ids with filter debug enabled. Only these need to be sent to the operator;
    /// everything else has debug off.
    pub fn debug_queues(&self) -> impl Iterator<Item = &str> {
        self.0
            .iter()
            .filter_map(|split| split.debug.then_some(split.queue_id.as_str()))
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

    /// Get all the Kafka queue ids from the config.
    pub fn kafka_queues(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::Kafka { .. } => Some(split.queue_id.as_str()),
            _ => None,
        })
    }

    /// Out of the whole queue splitting config, get only the kafka message header filters.
    pub fn kafka(&self) -> impl Iterator<Item = (&str, &QueueMessageFilter)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::Kafka {
                message_filter: Some(message_filter),
                ..
            } => Some((split.queue_id.as_str(), message_filter)),
            _ => None,
        })
    }

    /// Out of the whole queue splitting config, get only the kafka jq filters.
    pub fn kafka_jq_filters(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::Kafka {
                jq_filter: Some(jq),
                ..
            } => Some((split.queue_id.as_str(), jq.as_str())),
            _ => None,
        })
    }

    /// Out of the whole queue splitting config, get only the kafka protobuf payload decoding
    /// configs.
    pub fn kafka_payload_protobuf(&self) -> impl Iterator<Item = (&str, &KafkaPayloadProtobuf)> {
        self.0.iter().filter_map(|split| match &split.filter {
            QueueFilter::Kafka {
                payload_protobuf: Some(protobuf),
                ..
            } => Some((split.queue_id.as_str(), protobuf)),
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
                queue_name: queue_id.to_owned(),
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
                QueueFilter::Kafka {
                    message_filter,
                    jq_filter,
                    payload_protobuf,
                } => {
                    if let Some(filter) = message_filter {
                        Self::verify_message_attribute_filter(queue_name, filter)?;
                    }
                    if let Some(jq_filter) = jq_filter {
                        Self::verify_jq_program(queue_name, jq_filter)?;
                    }
                    // The decoded payload is only ever consumed by the jq program, so a
                    // protobuf config with no jq filter would silently decode into nothing.
                    if payload_protobuf.is_some() && jq_filter.is_none() {
                        return Err(QueueSplittingVerificationError::ProtobufWithoutJqFilter(
                            queue_name.clone(),
                        ));
                    }
                }
                QueueFilter::Rmq { message_filter } => {
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
            map.serialize_entry(
                &split.queue_id,
                &QueueSplitMapValue {
                    queue_mode: split.queue_mode,
                    debug: split.debug,
                    filter: &split.filter,
                },
            )?;
        }
        map.end()
    }
}

/// The value side of the map form: a filter tagged by `queue_type`, plus the optional
/// `queue_mode`. Keeping `queue_mode` here (rather than next to the queue id key) means the map and
/// list forms accept the same per-queue fields.
#[derive(Serialize)]
struct QueueSplitMapValue<'a> {
    #[serde(skip_serializing_if = "QueueMode::is_steal")]
    queue_mode: QueueMode,
    #[serde(skip_serializing_if = "Not::not")]
    debug: bool,
    #[serde(flatten)]
    filter: &'a QueueFilter,
}

/// Owned counterpart of [`QueueSplitMapValue`] used when reading the map form. Also drives the
/// JSON schema for the map value so `queue_mode` is documented alongside the filter.
#[derive(Deserialize, JsonSchema)]
struct QueueSplitMapValueOwned {
    #[serde(default, skip_serializing_if = "QueueMode::is_steal")]
    queue_mode: QueueMode,
    #[serde(default)]
    debug: bool,
    #[serde(flatten)]
    filter: QueueFilter,
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
                while let Some((queue_id, value)) =
                    map.next_entry::<QueueId, QueueSplitMapValueOwned>()?
                {
                    splits.push(QueueSplit {
                        queue_id,
                        queue_mode: value.queue_mode,
                        debug: value.debug,
                        filter: value.filter,
                    });
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
        let map_value = generator
            .subschema_for::<QueueSplitMapValueOwned>()
            .to_value();
        let split = generator.subschema_for::<QueueSplit>().to_value();

        let mut schema = schemars::json_schema!({});
        schema.insert(
            "anyOf".to_owned(),
            serde_json::json!([
                { "type": "object", "additionalProperties": map_value },
                { "type": "array", "items": split },
            ]),
        );
        schema
    }
}

impl MirrordConfig for SplitQueuesConfig {
    type Generated = Self;

    fn generate_config(
        mut self,
        _context: &mut ConfigContext,
    ) -> crate::config::Result<Self::Generated> {
        // Protobuf schemas are compiled here, on the local machine, because imports in the
        // `.proto` files can only be resolved against the local filesystem. Everything
        // downstream (connect params, the copy-target CRD) carries the compiled descriptor.
        for split in &mut self.0 {
            if let QueueFilter::Kafka {
                payload_protobuf: Some(protobuf),
                ..
            } = &mut split.filter
            {
                protobuf.resolve_descriptor(&split.queue_id)?;
            }
        }
        Ok(self)
    }
}

impl FromMirrordConfig for SplitQueuesConfig {
    type Generator = Self;
}

pub type QueueMessageFilter = BTreeMap<String, String>;

/// ### feature.split_queues.{}.payload_protobuf {#feature-split_queues-queue_id-payload_protobuf}
///
/// Only supported with `queue_type` of `Kafka`.
///
/// Decodes the raw protobuf bytes in the message payload before the `jq_filter` runs, for
/// topics that carry plain protobuf (for example CDC events) instead of JSON. The decoded
/// message is exposed to the jq program as an extra `payload_decoded` field, so filters can
/// target schema fields directly:
///
/// ```json
/// {
///   "queue_type": "Kafka",
///   "payload_protobuf": {
///     "schema_file": "schemas/cdc_record.proto",
///     "message_type": "com.example.cdc.Record"
///   },
///   "jq_filter": ".payload_decoded.merchant_id == 2137"
/// }
/// ```
///
/// The schema is compiled locally by the mirrord CLI (resolving imports on your machine), so
/// the operator never needs access to your `.proto` files. Field names appear in
/// `payload_decoded` exactly as written in the schema, enum values as their names, and 64-bit
/// integers as JSON numbers. Fields at their default value are included. Messages that fail to
/// decode with the given schema never match the filter and stay with the deployed application.
///
/// The payload must be plain protobuf: schema-registry framing (magic byte + schema id prefix)
/// is not supported.
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KafkaPayloadProtobuf {
    /// Path to the `.proto` file defining the payload's message type. Relative paths are
    /// resolved against the current working directory. Not needed when `descriptor_base64` is
    /// provided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_file: Option<PathBuf>,

    /// Extra import roots for compiling `schema_file`. The file's own directory is always an
    /// import root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_directories: Vec<PathBuf>,

    /// Fully-qualified name of the payload's message type, e.g. `com.example.cdc.Record`.
    pub message_type: String,

    /// Base64-encoded serialized `FileDescriptorSet`, optionally gzip-compressed (plain
    /// `protoc --descriptor_set_out --include_imports` output works as-is). An alternative to
    /// `schema_file` for pre-compiled schemas; filled in automatically from `schema_file`
    /// during config resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor_base64: Option<String>,
}

impl KafkaPayloadProtobuf {
    /// The resolved `descriptor_base64` is gzip-compressed: the descriptor travels to the
    /// operator inside the connect URL's query string, where proxies commonly cap URI and
    /// header sizes at a few KB, so every byte matters. Consumers sniff this magic to accept
    /// both compressed and plain descriptors, so a `protoc --descriptor_set_out` value pasted
    /// into the config keeps working.
    pub const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

    /// Makes this config self-contained before it leaves the local machine: compiles
    /// `schema_file` (or validates a user-provided `descriptor_base64`) into a serialized
    /// `FileDescriptorSet` stored gzipped in `descriptor_base64`, and checks that
    /// `message_type` exists in it. The operator only ever sees the descriptor, never the
    /// `.proto` files, so imports can be resolved against the local filesystem here.
    fn resolve_descriptor(
        &mut self,
        queue_id: &str,
    ) -> Result<(), QueueSplittingVerificationError> {
        let invalid = |error: String| QueueSplittingVerificationError::ProtobufDescriptorInvalid {
            queue_name: queue_id.to_owned(),
            error,
        };

        let descriptor = match (&self.descriptor_base64, &self.schema_file) {
            (Some(descriptor), _) => {
                let raw = BASE64_STANDARD
                    .decode(descriptor)
                    .map_err(|error| invalid(error.to_string()))?;
                if raw.starts_with(&Self::GZIP_MAGIC) {
                    let mut plain = Vec::new();
                    flate2::read::GzDecoder::new(raw.as_slice())
                        .read_to_end(&mut plain)
                        .map_err(|error| invalid(error.to_string()))?;
                    plain
                } else {
                    raw
                }
            }
            (None, Some(schema_file)) => {
                Self::compile_schema(queue_id, schema_file, &self.include_directories)?
            }
            (None, None) => {
                return Err(QueueSplittingVerificationError::ProtobufSchemaMissing {
                    queue_name: queue_id.to_owned(),
                });
            }
        };

        let pool = DescriptorPool::decode(descriptor.as_slice())
            .map_err(|error| invalid(error.to_string()))?;
        if pool.get_message_by_name(&self.message_type).is_none() {
            return Err(
                QueueSplittingVerificationError::ProtobufMessageTypeNotFound {
                    queue_name: queue_id.to_owned(),
                    message_type: self.message_type.clone(),
                },
            );
        }

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        encoder
            .write_all(&descriptor)
            .and_then(|()| encoder.finish())
            .map(|compressed| self.descriptor_base64 = Some(BASE64_STANDARD.encode(compressed)))
            .map_err(|error| invalid(error.to_string()))
    }

    fn compile_schema(
        queue_id: &str,
        schema_file: &Path,
        include_directories: &[PathBuf],
    ) -> Result<Vec<u8>, QueueSplittingVerificationError> {
        let compile_error = |error: String| QueueSplittingVerificationError::ProtobufCompile {
            queue_name: queue_id.to_owned(),
            schema_file: schema_file.display().to_string(),
            errors: error,
        };

        let file_directory = schema_file
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let includes = include_directories
            .iter()
            .map(PathBuf::as_path)
            .chain(std::iter::once(file_directory));

        // Unlike `protox::compile`, source info (comments and source spans) is excluded: the
        // filter only needs the type structure, and descriptor size matters because it rides
        // in the connect URL.
        let mut compiler =
            protox::Compiler::new(includes).map_err(|error| compile_error(error.to_string()))?;
        compiler.include_imports(true).include_source_info(false);
        compiler
            .open_file(schema_file)
            .map_err(|error| compile_error(error.to_string()))?;

        Ok(compiler.file_descriptor_set().encode_to_vec())
    }
}

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
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, JsonSchema, EnumDiscriminants)]
#[serde(tag = "queue_type", deny_unknown_fields)]
#[strum_discriminants(name(QueueKind))]
#[strum_discriminants(derive(Hash, PartialOrd, Ord, EnumIter))]
pub enum QueueFilter {
    /// ### feature.split_queues.{}.jq_filter {#feature-split_queues-queue_id-jq_filter}
    /// Not supported with `queue_type` of `RMQ`.
    /// When this field is specified, for each message, the jq filter runs on a JSON
    /// representation of the message. If the jq program outputs `true`, that
    /// message is considered as matching the filter.
    ///
    /// For **SQS**, [an SQS `Message` object](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/APIReference/API_Message.html)
    /// is used.
    ///
    /// For **GCP Pub/Sub**, the JSON representation of [`PubsubMessage`](https://cloud.google.com/pubsub/docs/reference/rest/v1/PubsubMessage)
    /// is used.
    ///
    /// For **Kafka**, an object with `topic`, `partition`, `offset`, `timestamp`, `key`,
    /// `payload`, and `headers` fields is used. `key`, `payload`, and header values are UTF-8
    /// strings, or base64-encoded when not valid UTF-8. With `payload_protobuf` set, the
    /// object additionally has a `payload_decoded` field holding the payload decoded from
    /// protobuf.
    ///
    /// For **Azure Service Bus**, an object with `body`, `application_properties`,
    /// `message_id`, `content_type`, and `subject` fields is used.
    ///
    /// For **Redis Pub/Sub**, the message payload parsed as JSON is used. Messages whose
    /// payload is not valid JSON never match.
    ///
    /// For **Temporal**, an object the operator builds for each task is used. Every object has
    /// a `task_type` field, set to either `"activity"` or `"workflow"`. Activity tasks also
    /// carry `workflow_namespace`, `workflow_id`, `run_id`, `workflow_type`, `activity_type`,
    /// `activity_id`, `attempt`, `header`, and `input` (an array of the decoded arguments).
    /// Workflow tasks also carry `workflow_id`, `run_id`, `workflow_type`, `attempt`,
    /// `task_queue`, `cron_schedule`, `identity`, `first_execution_run_id`, `header`,
    /// `search_attributes`, `memo`, and `input`.
    ///
    /// For **BullMQ**, the job's `data` field parsed as JSON is used. Jobs whose `data` is not
    /// valid JSON never match.
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
        #[serde(skip_serializing_if = "Option::is_none")]
        message_filter: Option<QueueMessageFilter>,

        /// A jq filter.
        ///
        /// When this is specified, for each Kafka message, the jq filter runs on a JSON
        /// representation of the message record: an object with `topic`, `partition`, `offset`,
        /// `timestamp` (milliseconds, present when the record has one), `key`, `payload`, and
        /// `headers` (an object mapping header names to their values) fields. `key`, `payload`,
        /// and header values are UTF-8 strings, or base64-encoded when not valid UTF-8.
        ///
        /// If the jq program outputs `true`, that message is considered as matching the filter.
        ///
        /// For example, `".payload | fromjson | .customer_id == 2137"` matches messages whose
        /// payload is a JSON object with a `customer_id` field equal to `2137`.
        ///
        /// When `payload_protobuf` is set, the object additionally has a `payload_decoded`
        /// field holding the payload decoded from protobuf, so the program can target schema
        /// fields directly, e.g. `".payload_decoded.merchant_id == 2137"`.
        #[serde(skip_serializing_if = "Option::is_none")]
        jq_filter: Option<String>,

        /// Decodes the raw protobuf payload into a `payload_decoded` field for `jq_filter`,
        /// for topics that carry plain protobuf instead of JSON.
        #[serde(skip_serializing_if = "Option::is_none")]
        payload_protobuf: Option<KafkaPayloadProtobuf>,
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
        /// object with `body`, `application_properties`, `message_id`, `content_type`, and
        /// `subject` fields.
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
        analytics.add("kafka_queue_count", self.kafka_queues().count());
        // The number of Kafka queues filtered with jq filters.
        analytics.add("kafka_jq_filter_count", self.kafka_jq_filters().count());
        // The number of Kafka queues with protobuf payload decoding.
        analytics.add(
            "kafka_protobuf_decoding_count",
            self.kafka_payload_protobuf().count(),
        );
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
    #[error(
        "{queue_name}.payload_protobuf: neither `schema_file` nor `descriptor_base64` is set - \
         set `schema_file` to the `.proto` file describing the topic's payload"
    )]
    ProtobufSchemaMissing { queue_name: String },
    #[error(
        "{queue_name}.payload_protobuf: failed to compile `{schema_file}`: {errors}. Check that \
         the file and everything it imports are reachable through `include_directories`"
    )]
    ProtobufCompile {
        queue_name: String,
        schema_file: String,
        errors: String,
    },
    #[error(
        "{queue_name}.payload_protobuf.descriptor_base64: not a valid base64-encoded \
         `FileDescriptorSet` ({error}) - generate one with `protoc --descriptor_set_out \
         --include_imports`, or set `schema_file` instead"
    )]
    ProtobufDescriptorInvalid { queue_name: String, error: String },
    #[error(
        "{queue_name}.payload_protobuf.message_type: message `{message_type}` not found in the \
         compiled schema - use the fully-qualified name, e.g. `com.example.MyRecord`"
    )]
    ProtobufMessageTypeNotFound {
        queue_name: String,
        message_type: String,
    },
    #[error(
        "{0}: `payload_protobuf` decodes the payload for `jq_filter`, which is not set - add a \
         `jq_filter` that uses `.payload_decoded`, or remove `payload_protobuf`"
    )]
    ProtobufWithoutJqFilter(String),
}

#[cfg(test)]
mod test {
    use super::{QueueFilter, QueueMode, QueueSplit, SplitQueuesConfig};
    use crate::{
        config::{ConfigContext, MirrordConfig},
        env_key::EnvKey,
    };

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
                message_filter: Some([("key".to_owned(), "value".to_owned())].into()),
                jq_filter: None,
                payload_protobuf: None,
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
                message_filter: [("key".to_owned(), "value".to_owned())].into(),
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
                message_filter: Some([("key".to_owned(), "value".to_owned())].into()),
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
                jq_filter: Some("whatever".to_owned()),
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
                jq_filter: Some("whatever".to_owned()),
                message_filter: Some([("who".to_owned(), "me".to_owned())].into()),
            }
        );
    }

    #[test]
    fn all_wildcard_covers_every_queue_type() {
        let key = EnvKey::Provided("zamek.bobolice".to_owned());
        let config = SplitQueuesConfig::all_wildcard(&key);

        config.verify(&mut ConfigContext::default()).unwrap();

        assert!(config.splits().iter().all(|split| split.queue_id == "*"));
        assert!(config.is_all_wildcard(&key));

        assert_eq!(config.rmq().count(), 0);
        assert_eq!(config.sqs().count(), 0);
        assert_eq!(config.kafka().count(), 0);
        assert_eq!(config.gcp_pubsub().count(), 0);
        assert_eq!(config.azure_service_bus().count(), 0);
        assert_eq!(config.redis_pubsub().count(), 0);
        assert_eq!(config.temporal().count(), 0);
        assert_eq!(config.bullmq().count(), 0);
        for jq_filters in [
            config.sqs_jq_filters().count(),
            config.kafka_jq_filters().count(),
            config.gcp_pubsub_jq_filters().count(),
            config.azure_service_bus_jq_filters().count(),
            config.redis_pubsub_jq_filters().count(),
            config.temporal_jq_filters().count(),
            config.bullmq_jq_filters().count(),
        ] {
            assert_eq!(jq_filters, 1);
        }
    }

    #[test]
    fn all_wildcard_jq_selectors() {
        let key = EnvKey::Provided("zamek.bobolice".to_owned());
        let config = SplitQueuesConfig::all_wildcard(&key);

        let selectors = [
            config.sqs_jq_filters().next().unwrap(),
            config.kafka_jq_filters().next().unwrap(),
            config.gcp_pubsub_jq_filters().next().unwrap(),
            config.azure_service_bus_jq_filters().next().unwrap(),
            config.redis_pubsub_jq_filters().next().unwrap(),
            config.temporal_jq_filters().next().unwrap(),
            config.bullmq_jq_filters().next().unwrap(),
        ];

        assert_eq!(
            selectors,
            [
                (
                    "*",
                    r#"(.MessageAttributes // {}) | [.. | select(type == "string" and contains("mirrord-session=zamek.bobolice"))] | length > 0"#
                ),
                (
                    "*",
                    r#"(.headers // {}) | [.. | select(type == "string" and contains("mirrord-session=zamek.bobolice"))] | length > 0"#
                ),
                (
                    "*",
                    r#"(.attributes // {}) | [.. | select(type == "string" and contains("mirrord-session=zamek.bobolice"))] | length > 0"#
                ),
                (
                    "*",
                    r#"(.application_properties // {}) | [.. | select(type == "string" and contains("mirrord-session=zamek.bobolice"))] | length > 0"#
                ),
                (
                    "*",
                    r#"(. // {}) | [.. | select(type == "string" and contains("mirrord-session=zamek.bobolice"))] | length > 0"#
                ),
                (
                    "*",
                    r#"(.header // {}) | [.. | select(type == "string" and contains("mirrord-session=zamek.bobolice"))] | length > 0"#
                ),
                (
                    "*",
                    r#"(. // {}) | [.. | select(type == "string" and contains("mirrord-session=zamek.bobolice"))] | length > 0"#
                ),
            ]
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
                queue_mode: QueueMode::Steal,
                debug: false,
                filter: QueueFilter::Sqs {
                    message_filter: Some([("k".to_owned(), "v".to_owned())].into()),
                    jq_filter: None,
                },
            },
            QueueSplit {
                queue_id: "second".to_owned(),
                queue_mode: QueueMode::Mirror,
                debug: false,
                filter: QueueFilter::Kafka {
                    message_filter: Some([("who".to_owned(), "you$".to_owned())].into()),
                    jq_filter: None,
                    payload_protobuf: None,
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
                queue_mode: QueueMode::Steal,
                debug: false,
                filter: QueueFilter::Sqs {
                    message_filter: Some([("region".to_owned(), "^eu".to_owned())].into()),
                    jq_filter: None,
                },
            },
            QueueSplit {
                queue_id: "orders".to_owned(),
                queue_mode: QueueMode::Mirror,
                debug: false,
                filter: QueueFilter::Kafka {
                    message_filter: Some([("region".to_owned(), "^us".to_owned())].into()),
                    jq_filter: None,
                    payload_protobuf: None,
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

    /// Writes a small CDC-style schema to a temp dir and returns a Kafka split entry
    /// pointing at it.
    fn protobuf_split(
        schema_dir: &std::path::Path,
        message_type: &str,
        jq_filter: Option<&str>,
    ) -> QueueSplit {
        std::fs::write(
            schema_dir.join("record.proto"),
            r#"syntax = "proto3";
            package test.cdc;
            message Metadata { string transactionType = 1; }
            message Record {
                string custom_record_identifier = 1;
                int64 merchant_id = 2;
                Metadata metadata = 3;
            }"#,
        )
        .unwrap();

        QueueSplit {
            queue_id: "cdc-topic".to_owned(),
            queue_mode: QueueMode::default(),
            debug: false,
            filter: QueueFilter::Kafka {
                message_filter: None,
                jq_filter: jq_filter.map(ToOwned::to_owned),
                payload_protobuf: Some(super::KafkaPayloadProtobuf {
                    schema_file: Some(schema_dir.join("record.proto")),
                    include_directories: Vec::new(),
                    message_type: message_type.to_owned(),
                    descriptor_base64: None,
                }),
            },
        }
    }

    /// Config generation must compile the schema file into an embedded descriptor, so
    /// everything downstream of the CLI is self-contained and never needs the `.proto`.
    #[test]
    fn payload_protobuf_generation_embeds_descriptor() {
        use base64::{Engine, prelude::BASE64_STANDARD};

        let dir = tempfile::tempdir().unwrap();
        let config = SplitQueuesConfig::from_splits([protobuf_split(
            dir.path(),
            "test.cdc.Record",
            Some(r#".payload_decoded.merchant_id == 2137"#),
        )]);

        let generated = config
            .generate_config(&mut ConfigContext::default())
            .unwrap();
        generated.verify(&mut ConfigContext::default()).unwrap();

        let (queue_id, protobuf) = generated.kafka_payload_protobuf().next().unwrap();
        assert_eq!(queue_id, "cdc-topic");
        // The embedded descriptor is stored gzipped (it rides in the connect URL, so size
        // matters); consumers sniff the magic and decompress.
        let compressed = BASE64_STANDARD
            .decode(protobuf.descriptor_base64.as_deref().unwrap())
            .unwrap();
        assert!(compressed.starts_with(&super::KafkaPayloadProtobuf::GZIP_MAGIC));
        let mut descriptor = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::GzDecoder::new(compressed.as_slice()),
            &mut descriptor,
        )
        .unwrap();
        let pool = prost_reflect::DescriptorPool::decode(descriptor.as_slice()).unwrap();
        assert!(pool.get_message_by_name("test.cdc.Record").is_some());

        // Re-resolving an already-resolved config (the compressed descriptor fed back in) must
        // be idempotent, not fail or double-compress.
        generated
            .clone()
            .generate_config(&mut ConfigContext::default())
            .unwrap();
    }

    #[test]
    fn payload_protobuf_generation_rejects_unknown_message_type() {
        let dir = tempfile::tempdir().unwrap();
        let config = SplitQueuesConfig::from_splits([protobuf_split(
            dir.path(),
            "test.cdc.Nope",
            Some(".payload_decoded.x == 1"),
        )]);

        config
            .generate_config(&mut ConfigContext::default())
            .unwrap_err();
    }

    /// A user can paste plain `protoc --descriptor_set_out` output into `descriptor_base64`
    /// instead of pointing at a schema file; resolution must accept it and store the
    /// compressed form.
    #[test]
    fn payload_protobuf_accepts_plain_user_supplied_descriptor() {
        use base64::{Engine, prelude::BASE64_STANDARD};

        let dir = tempfile::tempdir().unwrap();
        let mut split = protobuf_split(
            dir.path(),
            "test.cdc.Record",
            Some(".payload_decoded.merchant_id == 2137"),
        );
        let QueueFilter::Kafka {
            payload_protobuf: Some(protobuf),
            ..
        } = &mut split.filter
        else {
            unreachable!("protobuf_split always builds a Kafka filter with payload_protobuf");
        };
        let plain = protox::compile([dir.path().join("record.proto")], [dir.path()]).unwrap();
        protobuf.descriptor_base64 =
            Some(BASE64_STANDARD.encode(prost::Message::encode_to_vec(&plain)));
        protobuf.schema_file = None;

        let generated = SplitQueuesConfig::from_splits([split])
            .generate_config(&mut ConfigContext::default())
            .unwrap();

        let (_, resolved) = generated.kafka_payload_protobuf().next().unwrap();
        let stored = BASE64_STANDARD
            .decode(resolved.descriptor_base64.as_deref().unwrap())
            .unwrap();
        assert!(stored.starts_with(&super::KafkaPayloadProtobuf::GZIP_MAGIC));
    }

    #[test]
    fn payload_protobuf_without_jq_filter_fails_verification() {
        let dir = tempfile::tempdir().unwrap();
        let config =
            SplitQueuesConfig::from_splits([protobuf_split(dir.path(), "test.cdc.Record", None)]);

        let generated = config
            .generate_config(&mut ConfigContext::default())
            .unwrap();
        generated.verify(&mut ConfigContext::default()).unwrap_err();
    }
}
