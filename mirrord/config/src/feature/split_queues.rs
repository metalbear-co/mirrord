use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::{Read, Write},
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
    de::{self, MapAccess, SeqAccess, Visitor},
    ser::SerializeMap,
};
use strum::IntoEnumIterator;
use strum_macros::{Display, EnumIter};
use thiserror::Error;

use crate::{
    config::{ConfigContext, FromMirrordConfig, MirrordConfig},
    env_key::EnvKey,
};

pub type QueueId = String;

/// The legacy per-attribute filter: attribute name to the regex its value must match. Every entry
/// must match, so the map is an implicit `all_of` over exact attribute names.
pub type QueueMessageFilter = BTreeMap<String, String>;

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

/// ### feature.split_queues.{}.queue_type {#feature-split_queues-queue_id-queue_type}
///
/// The broker the queue lives on. One of `SQS`, `Kafka`, `RMQ`, `GCPPubSub`, `RedisPubSub`,
/// `AzureServiceBus`, `Temporal`, `BullMQ`, `NATS`, or `NATSPubSub`.
///
/// The `Display` form is the snake_case name used in analytics keys and logs.
#[derive(
    Serialize,
    Deserialize,
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    Hash,
    PartialOrd,
    Ord,
    JsonSchema,
    EnumIter,
    Display,
)]
pub enum QueueKind {
    #[serde(rename = "SQS")]
    #[strum(serialize = "sqs")]
    Sqs,
    #[serde(rename = "Kafka")]
    #[strum(serialize = "kafka")]
    Kafka,
    #[serde(rename = "RMQ")]
    #[strum(serialize = "rmq")]
    Rmq,
    #[serde(rename = "GCPPubSub")]
    #[strum(serialize = "gcp_pubsub")]
    GcpPubSub,
    #[serde(rename = "RedisPubSub")]
    #[strum(serialize = "redis_pubsub")]
    RedisPubSub,
    #[serde(rename = "AzureServiceBus")]
    #[strum(serialize = "azure_service_bus")]
    AzureServiceBus,
    #[serde(rename = "Temporal")]
    #[strum(serialize = "temporal")]
    Temporal,
    #[serde(rename = "BullMQ")]
    #[strum(serialize = "bullmq")]
    BullMq,
    #[serde(rename = "NATS")]
    #[strum(serialize = "nats")]
    Nats,
    #[serde(rename = "NATSPubSub")]
    #[strum(serialize = "nats_pubsub")]
    NatsPubSub,

    /// A queue type this version of mirrord does not know. Produced when an older operator reads
    /// a config written by a newer client; it never comes from a user config, which `verify`
    /// rejects with a clear error.
    #[schemars(skip)]
    #[serde(other)]
    #[strum(serialize = "unknown")]
    Unknown,
}

impl QueueKind {
    /// Every broker kind a user can name in the config, in a stable order.
    pub fn known() -> impl Iterator<Item = Self> {
        Self::iter().filter(|kind| *kind != Self::Unknown)
    }

    /// Whether `payload_protobuf` makes sense for this broker: only Kafka carries raw protobuf
    /// payloads the operator can decode before the jq filter runs.
    fn supports_payload_protobuf(self) -> bool {
        matches!(self, Self::Kafka)
    }
}

/// ### feature.split_queues.{}.filter {#feature-split_queues-queue_id-filter}
///
/// A composable message filter, shaped like the HTTP `http_filter`: one `metadata` regex, or an
/// `all_of` / `any_of` list of `metadata` regexes.
///
/// A `metadata` regex is matched against every message attribute (SQS message attributes, Kafka
/// and RabbitMQ headers, Pub/Sub attributes, Service Bus application properties, Temporal task
/// metadata, top-level JSON fields for Redis Pub/Sub and BullMQ) rendered as
/// `<name>: <value>`, the same way the HTTP filter sees headers. The message matches when any
/// attribute line matches, so one regex can target an attribute by name (`^tenant: blue$`) or
/// a value wherever it appears (`.*mirrord-session={{ key }}.*`). Matching is case sensitive.
///
/// Use `filter` **or** the older `message_filter`, not both. `message_filter` is a map from an
/// exact attribute name to a regex on its value, and is equivalent to an `all_of` of one
/// `metadata` filter per entry.
///
/// ```json
/// {
///   "feature": {
///     "split_queues": [
///       {
///         "queue_id": "*",
///         "queue_type": "SQS",
///         "filter": { "metadata": "^tenant: blue-.*$" }
///       },
///       {
///         "queue_id": "*",
///         "queue_type": "Temporal",
///         "filter": {
///           "any_of": [
///             { "metadata": "^header.baggage: .*mirrord-session={{ key }}.*$" },
///             { "metadata": "^header.test: .*mirrord-session={{ key }}.*$" }
///           ]
///         }
///       }
///     ]
///   }
/// }
/// ```
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum MessageFilterConfig {
    /// A regex matched against each message attribute rendered as `<name>: <value>`. Supports
    /// the syntax of the [`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/)
    /// crate.
    Metadata { metadata: String },

    /// The message must match every filter in the list. Cannot be empty.
    AllOf {
        #[schemars(length(min = 1))]
        all_of: Vec<InnerMessageFilter>,
    },

    /// The message must match at least one filter in the list. Cannot be empty.
    AnyOf {
        #[schemars(length(min = 1))]
        any_of: Vec<InnerMessageFilter>,
    },
}

/// One filter inside `all_of` / `any_of`. Only `metadata` regexes for now; the list form leaves
/// room for other leaf kinds without changing the shape.
#[derive(Serialize, Deserialize, Clone, Debug, Eq, PartialEq, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum InnerMessageFilter {
    /// A regex matched against each message attribute rendered as `<name>: <value>`.
    Metadata { metadata: String },
}

impl InnerMessageFilter {
    fn verify(&self, queue_id: &str, path: &str) -> Result<(), QueueSplittingVerificationError> {
        match self {
            Self::Metadata { metadata } => verify_metadata_pattern(queue_id, path, metadata),
        }
    }
}

/// Rejects an empty pattern and one `fancy_regex` cannot compile. `path` is where the pattern
/// sits in the config (for example `filter.any_of[1]`), so the error points at the entry to fix.
fn verify_metadata_pattern(
    queue_id: &str,
    path: &str,
    pattern: &str,
) -> Result<(), QueueSplittingVerificationError> {
    let path = format!("{path}.metadata");
    if pattern.is_empty() {
        return Err(QueueSplittingVerificationError::EmptyFilterPattern {
            queue_name: queue_id.to_owned(),
            path,
        });
    }
    Regex::new(pattern).map(drop).map_err(|error| {
        QueueSplittingVerificationError::InvalidRegex(queue_id.to_owned(), path, error.into())
    })
}

impl MessageFilterConfig {
    /// Checks every regex and rejects empty lists and patterns.
    fn verify(&self, queue_id: &str, path: &str) -> Result<(), QueueSplittingVerificationError> {
        match self {
            Self::Metadata { metadata } => verify_metadata_pattern(queue_id, path, metadata),
            Self::AllOf { all_of: filters } | Self::AnyOf { any_of: filters } => {
                let list = match self {
                    Self::AllOf { .. } => "all_of",
                    _ => "any_of",
                };
                if filters.is_empty() {
                    return Err(QueueSplittingVerificationError::EmptyCompositeFilter {
                        queue_name: queue_id.to_owned(),
                        path: format!("{path}.{list}"),
                    });
                }
                filters.iter().enumerate().try_for_each(|(index, filter)| {
                    filter.verify(queue_id, &format!("{path}.{list}[{index}]"))
                })
            }
        }
    }
}

/// The queue splitting configuration. Each entry pairs a queue id with a filter that decides which
/// messages from the original queue are delivered to the local application, based on message
/// attributes or headers, and possibly on jq filters (for SQS and other body-aware brokers).
///
/// The queue ids have to match those defined in the target's `MirrordSplitConfig` (or the legacy
/// `MirrordWorkloadQueueRegistry` / `MirrordKafkaTopicsConsumer`).
///
/// Two shapes are accepted. The classic map form keys each entry by its queue id, which means a
/// given id can appear only once:
///
/// ```json
/// {
///   "feature": {
///     "split_queues": {
///       "first-queue": {
///         "queue_type": "SQS",
///         "filter": { "metadata": "^wows: so wows$" }
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
///         "filter": { "metadata": "^region: eu" }
///       },
///       {
///         "queue_id": "orders",
///         "queue_type": "Kafka",
///         "filter": { "metadata": "^region: us" }
///       }
///     ]
///   }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct SplitQueuesConfig(Vec<QueueSplit>);

/// One queue to split: which broker it is on, which of its messages reach the local application,
/// and what happens to those messages. The same struct backs both config shapes: in the list form
/// it carries its `queue_id`, in the map form the id is the map key.
///
/// Adding a broker is one [`QueueKind`] variant; adding a filter option is one field here, read by
/// every broker through the operator's shared filter code.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueueSplit {
    /// ### feature.split_queues.{}.queue_id {#feature-split_queues-queue_id-queue_id}
    ///
    /// The id of the queue to split, as it appears in the target's split configuration. List form
    /// only: in the map form the id is the key. Does not have to be unique across entries. Use
    /// `*` to split every queue of the given `queue_type` with this filter.
    ///
    /// Empty only while a map-form entry is being read, before the key is copied in.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub queue_id: QueueId,

    /// Whether matched messages are stolen from the deployed application or mirrored to it.
    #[serde(default, skip_serializing_if = "QueueMode::is_steal")]
    pub queue_mode: QueueMode,

    /// The broker this queue lives on.
    pub queue_type: QueueKind,

    /// ### feature.split_queues.{}.message_filter {#feature-split_queues-queue_id-message_filter}
    ///
    /// The older filter shape: a mapping between message attribute (or header) names and regexes
    /// their values should match. The local application only receives messages that have
    /// **all** of the named attributes, each matching its pattern. Still supported; new configs
    /// should prefer `filter`, which can also match attributes without naming them and compose
    /// with `any_of` / `all_of`.
    ///
    /// For Temporal the names are `workflow_id`, `workflow_type`, `activity_type`,
    /// `header.<name>`, or a search attribute key. For Redis Pub/Sub and BullMQ they are
    /// top-level fields of the JSON payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_filter: Option<QueueMessageFilter>,

    /// The composable filter. See `feature.split_queues.{}.filter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<MessageFilterConfig>,

    /// ### feature.split_queues.{}.jq_filter {#feature-split_queues-queue_id-jq_filter}
    ///
    /// When this field is specified, for each message, the jq filter runs on a JSON
    /// representation of the message. If the jq program outputs `true`, that
    /// message is considered as matching the filter. Combined with `filter` or
    /// `message_filter`, a message must match both.
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
    /// For **RabbitMQ**, an object with `headers` (the AMQP basic-properties headers table),
    /// `properties`, and `payload` fields is used. The `payload` and header values are UTF-8
    /// strings, or base64-encoded when not valid UTF-8.
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
    /// For **NATS** and **NATSPubSub**, an object with `subject`, `headers`, and `payload`
    /// fields is used. `payload` is the message body parsed as JSON when the body is JSON, and
    /// a string otherwise (base64-encoded when not valid UTF-8). Unlike `NATS` (JetStream),
    /// core NATS pub/sub stores nothing, so delivery to the local application is best-effort:
    /// messages published while the split is being set up or torn down are not replayed.
    ///
    /// This can be used to filter messages based on their body content, for example.
    ///
    /// This filter, for example, will tell mirrord to only make available to this local
    /// application messages with a json in the message body, with a `customer_email` field
    /// that contains "metalbear.com": `".Body | fromjson | .customer_email |
    /// test(\"metalbear\\\\.com\")"`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jq_filter: Option<String>,

    /// Decodes the raw protobuf payload into a `payload_decoded` field for `jq_filter`, for
    /// Kafka topics that carry plain protobuf instead of JSON. See
    /// `feature.split_queues.{}.payload_protobuf`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_protobuf: Option<KafkaPayloadProtobuf>,
}

impl QueueSplit {
    /// A split of `queue_id` on `queue_type` with no filter, which delivers nothing to the local
    /// application until a filter is set.
    pub fn new(queue_id: impl Into<QueueId>, queue_type: QueueKind) -> Self {
        Self {
            queue_id: queue_id.into(),
            queue_mode: QueueMode::default(),
            queue_type,
            message_filter: None,
            filter: None,
            jq_filter: None,
            payload_protobuf: None,
        }
    }

    /// Whether this entry uses the composable `filter` shape, which older operators cannot read.
    pub fn has_composed_filter(&self) -> bool {
        self.filter.is_some()
    }

    fn verify(&self) -> Result<(), QueueSplittingVerificationError> {
        let queue_name = &self.queue_id;

        if self.queue_type == QueueKind::Unknown {
            return Err(QueueSplittingVerificationError::UnknownQueueType(
                queue_name.clone(),
            ));
        }

        if self.message_filter.is_some() && self.filter.is_some() {
            return Err(QueueSplittingVerificationError::FilterShapeConflict(
                queue_name.clone(),
            ));
        }

        if let Some(message_filter) = &self.message_filter {
            for (name, pattern) in message_filter {
                Regex::new(pattern).map_err(|error| {
                    QueueSplittingVerificationError::InvalidRegex(
                        queue_name.clone(),
                        format!("message_filter.{name}"),
                        error.into(),
                    )
                })?;
            }
        }

        if let Some(filter) = &self.filter {
            filter.verify(queue_name, "filter")?;
        }

        if let Some(jq_filter) = &self.jq_filter {
            SplitQueuesConfig::verify_jq_program(queue_name, jq_filter)?;
        }

        if self.payload_protobuf.is_some() {
            if !self.queue_type.supports_payload_protobuf() {
                return Err(
                    QueueSplittingVerificationError::ProtobufOnUnsupportedQueueType {
                        queue_name: queue_name.clone(),
                        queue_type: self.queue_type,
                    },
                );
            }
            // The decoded payload is only ever consumed by the jq program, so a protobuf config
            // with no jq filter would silently decode into nothing.
            if self.jq_filter.is_none() {
                return Err(QueueSplittingVerificationError::ProtobufWithoutJqFilter(
                    queue_name.clone(),
                ));
            }
        }

        Ok(())
    }
}

impl SplitQueuesConfig {
    pub fn from_splits(splits: impl IntoIterator<Item = QueueSplit>) -> Self {
        Self(splits.into_iter().collect())
    }

    /// Writes a [`SplitQueuesConfig`] with every jq-capable queue type where `queue_id = *` and
    /// default `queue_mode` (`Steal`).
    ///
    /// Mainly for `mirrord up`, so the user doesn't have to configure any queue splitting stuff, it
    /// gets handled by the operator instead.
    ///
    /// Queue types the operator has disabled are dropped on its side instead of failing the
    /// session, so listing all of them here is safe.
    pub fn all_wildcard_default_mode(key: &EnvKey) -> Self {
        Self::all_wildcard_with_mode(key, QueueMode::default())
    }

    /// Writes a [`SplitQueuesConfig`] with every jq-capable queue type where `queue_id = *`.
    ///
    /// Mainly for `mirrord up`, so the user doesn't have to configure any queue splitting stuff, it
    /// gets handled by the operator instead.
    ///
    /// Queue types the operator has disabled are dropped on its side instead of failing the
    /// session, so listing all of them here is safe. Core NATS pub/sub is left out on purpose:
    /// its delivery is best-effort, so it is only split when the user asks for it.
    pub fn all_wildcard_with_mode(key: &EnvKey, queue_mode: QueueMode) -> Self {
        // Each broker's jq selector points at the metadata object the session marker is
        // propagated through.
        const SESSION_MARKER_SELECTORS: [(QueueKind, &str); 9] = [
            (QueueKind::Sqs, ".MessageAttributes"),
            (QueueKind::Kafka, ".headers"),
            (QueueKind::Rmq, ".headers"),
            (QueueKind::GcpPubSub, ".attributes"),
            (QueueKind::AzureServiceBus, ".application_properties"),
            (QueueKind::RedisPubSub, "."),
            (QueueKind::Temporal, ".header"),
            (QueueKind::BullMq, "."),
            (QueueKind::Nats, ".headers"),
        ];

        Self::from_splits(
            SESSION_MARKER_SELECTORS
                .into_iter()
                .map(|(queue_type, selector)| QueueSplit {
                    queue_mode,
                    jq_filter: Some(Self::session_key_string_value_jq(selector, key)),
                    ..QueueSplit::new("*", queue_type)
                }),
        )
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

    pub fn is_all_wildcard_default_mode(&self, key: &EnvKey) -> bool {
        self == &Self::all_wildcard_default_mode(key)
    }

    pub fn is_all_wildcard_with_mode(&self, key: &EnvKey, queue_mode: QueueMode) -> bool {
        self == &Self::all_wildcard_with_mode(key, queue_mode)
    }

    /// Returns whether this configuration contains any queue at all.
    pub fn is_set(&self) -> bool {
        !self.0.is_empty()
    }

    /// All the queue splitting entries.
    pub fn splits(&self) -> &[QueueSplit] {
        &self.0
    }

    /// The entries for one broker kind.
    pub fn of_kind(&self, kind: QueueKind) -> impl Iterator<Item = &QueueSplit> {
        self.0.iter().filter(move |split| split.queue_type == kind)
    }

    /// Every broker kind that has at least one entry.
    pub fn kinds(&self) -> BTreeSet<QueueKind> {
        self.0.iter().map(|split| split.queue_type).collect()
    }

    /// Whether any entry uses the composable `filter` shape, which older operators cannot read.
    pub fn uses_composed_filters(&self) -> bool {
        self.0.iter().any(QueueSplit::has_composed_filter)
    }

    /// Queue ids whose mode is not the default `steal`, paired with their mode. Only these need to
    /// be sent to the operator; everything else is `steal`.
    pub fn queue_modes(&self) -> impl Iterator<Item = (&str, QueueMode)> {
        self.0.iter().filter_map(|split| {
            (!split.queue_mode.is_steal()).then_some((split.queue_id.as_str(), split.queue_mode))
        })
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
        self.0.iter().try_for_each(QueueSplit::verify)
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
            // The id is the map key, so the value is the entry without it.
            let value = QueueSplit {
                queue_id: QueueId::new(),
                ..split.clone()
            };
            map.serialize_entry(&split.queue_id, &value)?;
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
                while let Some((queue_id, mut split)) = map.next_entry::<QueueId, QueueSplit>()? {
                    if !split.queue_id.is_empty() {
                        return Err(de::Error::custom(format!(
                            "split_queues.{queue_id}: `queue_id` is the map key here, use the \
                             list form to put it inside the entry"
                        )));
                    }
                    split.queue_id = queue_id;
                    splits.push(split);
                }
                Ok(SplitQueuesConfig(splits))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut splits = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(split) = seq.next_element::<QueueSplit>()? {
                    if split.queue_id.is_empty() {
                        return Err(de::Error::custom(format!(
                            "split_queues[{}]: missing `queue_id`",
                            splits.len()
                        )));
                    }
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
        // Both shapes are the same entry schema; the list form requires the id inside the entry,
        // the map form has it as the key and must not repeat it inside.
        let mut list_entry = QueueSplit::json_schema(generator);
        if let Some(required) = list_entry
            .get_mut("required")
            .and_then(serde_json::Value::as_array_mut)
        {
            required.push("queue_id".into());
        }

        let mut map_value = QueueSplit::json_schema(generator);
        if let Some(properties) = map_value
            .get_mut("properties")
            .and_then(serde_json::Value::as_object_mut)
        {
            properties.remove("queue_id");
        }

        let mut schema = schemars::json_schema!({});
        schema.insert(
            "anyOf".to_owned(),
            serde_json::json!([
                { "type": "object", "additionalProperties": map_value.to_value() },
                { "type": "array", "items": list_entry.to_value() },
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
            if let Some(protobuf) = &mut split.payload_protobuf {
                protobuf.resolve_descriptor(&split.queue_id)?;
            }
        }
        Ok(self)
    }
}

impl FromMirrordConfig for SplitQueuesConfig {
    type Generator = Self;
}

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

impl CollectAnalytics for &SplitQueuesConfig {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        for kind in QueueKind::known() {
            let splits: Vec<_> = self.of_kind(kind).collect();
            analytics.add(format!("{kind}_queue_count"), splits.len());
            analytics.add(
                format!("{kind}_jq_filter_count"),
                splits.iter().filter(|s| s.jq_filter.is_some()).count(),
            );
        }
        // The number of SQS queues filtered with message attribute filters.
        analytics.add(
            "sqs_message_attr_filter_queue_count",
            self.of_kind(QueueKind::Sqs)
                .filter(|s| s.message_filter.is_some())
                .count(),
        );
        // The number of Kafka queues with protobuf payload decoding.
        analytics.add(
            "kafka_protobuf_decoding_count",
            self.of_kind(QueueKind::Kafka)
                .filter(|s| s.payload_protobuf.is_some())
                .count(),
        );
        // The number of queues using the composable `filter` shape, across brokers.
        analytics.add(
            "composed_filter_queue_count",
            self.splits()
                .iter()
                .filter(|s| s.has_composed_filter())
                .count(),
        );
    }
}

#[derive(Error, Debug)]
pub enum QueueSplittingVerificationError {
    #[error("{0}: unknown queue type")]
    UnknownQueueType(String),
    #[error("{0}.{1}: failed to parse regular expression ({2})")]
    InvalidRegex(
        String,
        String,
        // without `Box`, clippy complains when `ConfigError` is used in `Err`
        Box<fancy_regex::Error>,
    ),
    #[error(
        "{0}: both `filter` and `message_filter` are set - keep one of them; a `message_filter` \
         of `{{\"name\": \"pattern\"}}` is the same as `filter: {{\"metadata\": \"^name: \
         pattern\"}}`"
    )]
    FilterShapeConflict(String),
    #[error("{queue_name}.{path}: must list at least one filter")]
    EmptyCompositeFilter { queue_name: String, path: String },
    #[error("{queue_name}.{path}: the pattern is empty")]
    EmptyFilterPattern { queue_name: String, path: String },
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
    #[error(
        "{queue_name}: `payload_protobuf` is only supported for `queue_type: Kafka`, not \
         `{queue_type}` - remove it from this entry"
    )]
    ProtobufOnUnsupportedQueueType {
        queue_name: String,
        queue_type: QueueKind,
    },
}

#[cfg(test)]
mod test {
    use super::{
        InnerMessageFilter, MessageFilterConfig, QueueKind, QueueMode, QueueSplit,
        QueueSplittingVerificationError, SplitQueuesConfig,
    };
    use crate::{
        config::{ConfigContext, MirrordConfig},
        env_key::EnvKey,
    };

    fn message_filter(entries: &[(&str, &str)]) -> Option<super::QueueMessageFilter> {
        Some(
            entries
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        )
    }

    fn metadata(pattern: &str) -> MessageFilterConfig {
        MessageFilterConfig::Metadata {
            metadata: pattern.to_owned(),
        }
    }

    fn inner(pattern: &str) -> InnerMessageFilter {
        InnerMessageFilter::Metadata {
            metadata: pattern.to_owned(),
        }
    }

    fn verify(config: &SplitQueuesConfig) -> Result<(), QueueSplittingVerificationError> {
        config.verify(&mut ConfigContext::default())
    }

    #[test]
    fn deserialize_known_queue_types() {
        for (queue_type, kind) in [
            ("Kafka", QueueKind::Kafka),
            ("RMQ", QueueKind::Rmq),
            ("SQS", QueueKind::Sqs),
            ("NATSPubSub", QueueKind::NatsPubSub),
        ] {
            let value = serde_json::json!({
                "queue_type": queue_type,
                "message_filter": { "key": "value" },
            });

            let split = serde_json::from_value::<QueueSplit>(value).unwrap();
            assert_eq!(
                split,
                QueueSplit {
                    message_filter: message_filter(&[("key", "value")]),
                    ..QueueSplit::new("", kind)
                }
            );
        }
    }

    #[test]
    fn deserialize_unknown_queue_type() {
        let value = serde_json::json!({
            "queue_type": "unknown",
            "message_filter": { "key": "value" }
        });

        let split = serde_json::from_value::<QueueSplit>(value).unwrap();
        assert_eq!(split.queue_type, QueueKind::Unknown);
        verify(&SplitQueuesConfig::from_splits([split])).unwrap_err();
    }

    #[test]
    fn deserialize_sqs_with_both_jq_filter_and_attribute_filter() {
        let value = serde_json::json!({
            "queue_type": "SQS",
            "jq_filter": "whatever",
            "message_filter": { "who": "me" }
        });

        let split = serde_json::from_value::<QueueSplit>(value).unwrap();
        assert_eq!(
            split,
            QueueSplit {
                jq_filter: Some("whatever".to_owned()),
                message_filter: message_filter(&[("who", "me")]),
                ..QueueSplit::new("", QueueKind::Sqs)
            }
        );
    }

    /// The composable shape from the ticket: a single metadata regex, and an `any_of` of two.
    #[test]
    fn deserialize_composed_filters() {
        let value = serde_json::json!([
            {
                "queue_id": "*",
                "queue_type": "SQS",
                "filter": { "metadata": "^tenant: blue-.*$" }
            },
            {
                "queue_id": "*",
                "queue_type": "Temporal",
                "filter": {
                    "any_of": [
                        { "metadata": "^baggage: .*mirrord-session=abc.*$" },
                        { "metadata": "^test: .*mirrord-session=abc.*$" }
                    ]
                }
            }
        ]);

        let config = serde_json::from_value::<SplitQueuesConfig>(value).unwrap();
        verify(&config).unwrap();
        assert!(config.uses_composed_filters());
        assert_eq!(
            config.splits().first().and_then(|s| s.filter.clone()),
            Some(metadata("^tenant: blue-.*$"))
        );
        assert_eq!(
            config.splits().get(1).and_then(|s| s.filter.clone()),
            Some(MessageFilterConfig::AnyOf {
                any_of: vec![
                    inner("^baggage: .*mirrord-session=abc.*$"),
                    inner("^test: .*mirrord-session=abc.*$"),
                ]
            })
        );
    }

    /// A filter node must be exactly one of `metadata`, `all_of`, `any_of`.
    #[test]
    fn composed_filter_rejects_unknown_and_mixed_nodes() {
        for node in [
            serde_json::json!({ "header": "^x: y$" }),
            serde_json::json!({ "metadata": "^x: y$", "any_of": [] }),
            serde_json::json!({}),
            // Lists take `metadata` leaves only, like the HTTP filter's `all_of` / `any_of`.
            serde_json::json!({ "any_of": [ { "all_of": [ { "metadata": "^x: y$" } ] } ] }),
        ] {
            let value = serde_json::json!({ "queue_type": "SQS", "filter": node });
            serde_json::from_value::<QueueSplit>(value).unwrap_err();
        }
    }

    #[test]
    fn verify_rejects_both_filter_shapes_on_one_entry() {
        let config = SplitQueuesConfig::from_splits([QueueSplit {
            message_filter: message_filter(&[("tenant", "^blue$")]),
            filter: Some(metadata("^tenant: blue$")),
            ..QueueSplit::new("orders", QueueKind::Sqs)
        }]);

        let error = verify(&config).unwrap_err();
        assert!(
            matches!(error, QueueSplittingVerificationError::FilterShapeConflict(ref id) if id == "orders"),
            "{error}"
        );
    }

    #[test]
    fn verify_rejects_empty_composites_and_patterns_with_their_path() {
        let empty_list = SplitQueuesConfig::from_splits([QueueSplit {
            filter: Some(MessageFilterConfig::AnyOf { any_of: vec![] }),
            ..QueueSplit::new("orders", QueueKind::Sqs)
        }]);
        let error = verify(&empty_list).unwrap_err().to_string();
        assert!(error.contains("orders.filter.any_of"), "{error}");

        let empty_pattern = SplitQueuesConfig::from_splits([QueueSplit {
            filter: Some(MessageFilterConfig::AllOf {
                all_of: vec![inner("^a: b$"), inner("")],
            }),
            ..QueueSplit::new("orders", QueueKind::Sqs)
        }]);
        let error = verify(&empty_pattern).unwrap_err().to_string();
        assert!(
            error.contains("orders.filter.all_of[1].metadata"),
            "{error}"
        );
    }

    #[test]
    fn verify_rejects_invalid_regex_in_either_shape() {
        let composed = SplitQueuesConfig::from_splits([QueueSplit {
            filter: Some(metadata("^tenant: (blue$")),
            ..QueueSplit::new("orders", QueueKind::Sqs)
        }]);
        let error = verify(&composed).unwrap_err().to_string();
        assert!(error.contains("orders.filter.metadata"), "{error}");

        let legacy = SplitQueuesConfig::from_splits([QueueSplit {
            message_filter: message_filter(&[("tenant", "(blue")]),
            ..QueueSplit::new("orders", QueueKind::Sqs)
        }]);
        let error = verify(&legacy).unwrap_err().to_string();
        assert!(error.contains("orders.message_filter.tenant"), "{error}");
    }

    #[test]
    fn verify_rejects_payload_protobuf_outside_kafka() {
        let config = SplitQueuesConfig::from_splits([QueueSplit {
            jq_filter: Some(".payload_decoded.x == 1".to_owned()),
            payload_protobuf: Some(super::KafkaPayloadProtobuf {
                schema_file: None,
                include_directories: Vec::new(),
                message_type: "a.B".to_owned(),
                descriptor_base64: Some("AA==".to_owned()),
            }),
            ..QueueSplit::new("orders", QueueKind::Sqs)
        }]);

        let error = verify(&config).unwrap_err();
        assert!(
            matches!(
                error,
                QueueSplittingVerificationError::ProtobufOnUnsupportedQueueType { .. }
            ),
            "{error}"
        );
    }

    #[test]
    fn all_wildcard_covers_every_queue_type() {
        let key = EnvKey::Provided("zamek.bobolice".to_owned());
        let config = SplitQueuesConfig::all_wildcard_default_mode(&key);

        verify(&config).unwrap();

        assert!(config.splits().iter().all(|split| split.queue_id == "*"));
        assert!(config.is_all_wildcard_default_mode(&key));
        assert!(!config.uses_composed_filters());

        // Every jq-capable broker but core NATS pub/sub, each with only a jq filter. The order is
        // part of the contract: `mirrord up` copy-target specs serialize this as a list and
        // copy reuse compares specs, so reordering would stop new CLIs from reusing copies made
        // by older ones.
        assert_eq!(
            config
                .splits()
                .iter()
                .map(|s| s.queue_type)
                .collect::<Vec<_>>(),
            [
                QueueKind::Sqs,
                QueueKind::Kafka,
                QueueKind::Rmq,
                QueueKind::GcpPubSub,
                QueueKind::AzureServiceBus,
                QueueKind::RedisPubSub,
                QueueKind::Temporal,
                QueueKind::BullMq,
                QueueKind::Nats,
            ]
        );
        let mut every_known = QueueKind::known().collect::<Vec<_>>();
        every_known.retain(|kind| *kind != QueueKind::NatsPubSub);
        assert_eq!(config.kinds().len(), every_known.len());
        for split in config.splits() {
            assert!(split.message_filter.is_none() && split.filter.is_none());
            assert!(split.jq_filter.is_some());
        }
    }

    #[test]
    fn all_wildcard_jq_selectors() {
        let key = EnvKey::Provided("zamek.bobolice".to_owned());
        let config = SplitQueuesConfig::all_wildcard_default_mode(&key);

        let selectors = config
            .splits()
            .iter()
            .map(|split| (split.queue_type, split.jq_filter.as_deref().unwrap()))
            .collect::<Vec<_>>();

        let marker = r#"[.. | select(type == "string" and contains("mirrord-session=zamek.bobolice"))] | length > 0"#;
        let expected = [
            (QueueKind::Sqs, ".MessageAttributes"),
            (QueueKind::Kafka, ".headers"),
            (QueueKind::Rmq, ".headers"),
            (QueueKind::GcpPubSub, ".attributes"),
            (QueueKind::AzureServiceBus, ".application_properties"),
            (QueueKind::RedisPubSub, "."),
            (QueueKind::Temporal, ".header"),
            (QueueKind::BullMq, "."),
            (QueueKind::Nats, ".headers"),
        ]
        .map(|(kind, selector)| (kind, format!("({selector} // {{}}) | {marker}")));
        assert_eq!(
            selectors,
            expected
                .iter()
                .map(|(kind, jq)| (*kind, jq.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn deserialize_legacy_map_form() {
        let value = serde_json::json!({
            "first": { "queue_type": "SQS", "message_filter": { "k": "v" } },
            "second": { "queue_type": "Kafka", "message_filter": { "who": "you$" } },
        });

        let config = serde_json::from_value::<SplitQueuesConfig>(value).unwrap();
        let ids = |kind| {
            config
                .of_kind(kind)
                .map(|s| s.queue_id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(QueueKind::Sqs), ["first"]);
        assert_eq!(ids(QueueKind::Kafka), ["second"]);
    }

    /// In the map form the id is the key; repeating it inside the entry is a mistake we name
    /// rather than silently overwrite.
    #[test]
    fn map_form_rejects_queue_id_inside_entry() {
        let value = serde_json::json!({
            "first": { "queue_id": "other", "queue_type": "SQS" },
        });

        let error = serde_json::from_value::<SplitQueuesConfig>(value)
            .unwrap_err()
            .to_string();
        assert!(error.contains("`queue_id` is the map key"), "{error}");
    }

    #[test]
    fn deserialize_list_form() {
        let value = serde_json::json!([
            { "queue_id": "first", "queue_type": "SQS", "message_filter": { "k": "v" } },
            { "queue_id": "second", "queue_type": "Kafka", "message_filter": { "who": "you$" } },
        ]);

        let config = serde_json::from_value::<SplitQueuesConfig>(value).unwrap();
        assert_eq!(config.splits().len(), 2);
        assert_eq!(
            config.splits().first().map(|s| s.queue_id.as_str()),
            Some("first")
        );
    }

    #[test]
    fn list_form_requires_queue_id() {
        let value = serde_json::json!([{ "queue_type": "SQS" }]);

        let error = serde_json::from_value::<SplitQueuesConfig>(value)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("split_queues[0]: missing `queue_id`"),
            "{error}"
        );
    }

    /// The whole point of the list form: the same queue id used for two different brokers.
    #[test]
    fn deserialize_list_form_duplicate_id_across_brokers() {
        let value = serde_json::json!([
            { "queue_id": "orders", "queue_type": "SQS", "message_filter": { "region": "^eu" } },
            { "queue_id": "orders", "queue_type": "Kafka", "message_filter": { "region": "^us" } },
        ]);

        let config = serde_json::from_value::<SplitQueuesConfig>(value).unwrap();
        assert_eq!(config.kinds().len(), 2);
        assert!(config.splits().iter().all(|s| s.queue_id == "orders"));
    }

    /// Unique ids round-trip through the map form; duplicate ids round-trip through the list form.
    /// Both must deserialize back to the same config.
    #[test]
    fn serialize_round_trip() {
        let unique = SplitQueuesConfig::from_splits([
            QueueSplit {
                message_filter: message_filter(&[("k", "v")]),
                ..QueueSplit::new("first", QueueKind::Sqs)
            },
            QueueSplit {
                queue_mode: QueueMode::Mirror,
                filter: Some(metadata("^who: you$")),
                ..QueueSplit::new("second", QueueKind::Kafka)
            },
        ]);
        let json = serde_json::to_value(&unique).unwrap();
        assert!(json.is_object(), "unique ids should serialize as a map");
        assert_eq!(
            serde_json::from_value::<SplitQueuesConfig>(json).unwrap(),
            unique
        );

        let duplicate = SplitQueuesConfig::from_splits([
            QueueSplit {
                message_filter: message_filter(&[("region", "^eu")]),
                ..QueueSplit::new("orders", QueueKind::Sqs)
            },
            QueueSplit {
                queue_mode: QueueMode::Mirror,
                message_filter: message_filter(&[("region", "^us")]),
                ..QueueSplit::new("orders", QueueKind::Kafka)
            },
        ]);
        let json = serde_json::to_value(&duplicate).unwrap();
        assert!(json.is_array(), "duplicate ids should serialize as a list");
        assert_eq!(
            serde_json::from_value::<SplitQueuesConfig>(json).unwrap(),
            duplicate
        );
    }

    /// A legacy config must serialize to exactly the bytes older mirrord versions produced: the
    /// copy-target CRD carries this config verbatim, older operators read it with
    /// `deny_unknown_fields`, and copy-target reuse compares specs. A new field that leaks into
    /// the legacy shape would break all three.
    #[test]
    fn legacy_config_serializes_byte_identically() {
        let value = serde_json::json!({
            "orders": {
                "queue_type": "SQS",
                "message_filter": { "tenant": "^blue$" },
                "jq_filter": ".Body | fromjson | .x == 1"
            },
            "events": {
                "queue_mode": "mirror",
                "queue_type": "Kafka",
                "message_filter": { "who": "you$" }
            }
        });

        let config = serde_json::from_value::<SplitQueuesConfig>(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&config).unwrap(), value);
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
            jq_filter: jq_filter.map(ToOwned::to_owned),
            payload_protobuf: Some(super::KafkaPayloadProtobuf {
                schema_file: Some(schema_dir.join("record.proto")),
                include_directories: Vec::new(),
                message_type: message_type.to_owned(),
                descriptor_base64: None,
            }),
            ..QueueSplit::new("cdc-topic", QueueKind::Kafka)
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

        let split = generated.splits().first().unwrap();
        assert_eq!(split.queue_id, "cdc-topic");
        let protobuf = split.payload_protobuf.as_ref().unwrap();
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
        let protobuf = split
            .payload_protobuf
            .as_mut()
            .expect("protobuf_split always sets payload_protobuf");
        let plain = protox::compile([dir.path().join("record.proto")], [dir.path()]).unwrap();
        protobuf.descriptor_base64 =
            Some(BASE64_STANDARD.encode(prost::Message::encode_to_vec(&plain)));
        protobuf.schema_file = None;

        let generated = SplitQueuesConfig::from_splits([split])
            .generate_config(&mut ConfigContext::default())
            .unwrap();

        let resolved = generated
            .splits()
            .first()
            .and_then(|s| s.payload_protobuf.as_ref())
            .unwrap();
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
