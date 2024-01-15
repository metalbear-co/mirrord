use std::collections::HashMap;

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::{self, Deserialize, Serialize};

use crate::config::source::MirrordConfigSource;

/// Maps name of a `MirrordQueueSplitter` resource to a mapping from a fork name to a filter
/// definition.
/// ```json
/// {
///   "feature": {
///     "split_queues": {
///       "splitter": "whatever-q-splitter"
///       "queues": {
///         "first-queue": {
///           "SQS": {
///             "wows": {
///               "number_attribute": [
///                 { "greater_than_int": 2 },
///                 { "lesser_than_int": 6 }
///               ]
///             },
///             "coolz": {
///               "string_attribute": "^very .*"
///             }
///           }
///         }
///       }
///     }
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug)]
#[config(derive = "JsonSchema,Eq,PartialEq")]
pub struct SplitQueuesConfig {
    /// The name of the `MirrordQueueSplitter` resource to get queue details from.
    splitter: String,
    queues: HashMap<String, QueueMessageFilter>,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "queue_type")]
enum QueueMessageFilter {
    SQS(HashMap<String, SQSAttributeRuleSet>),
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
/// Set of rules to filter that are required to apply to a single field
enum SQSAttributeRuleSet {
    /// A single regex is enough for complex rules for a string.
    StringAttribute(String),

    NumberAttribute(Vec<NumberRule>),

    BinaryAttribute(Vec<BinaryAttributeRule>),
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
enum NumberRule {
    GreaterThanInt(i64),
    LesserThanInt(i64),
    // GreaterThanFloat(f64),
    // LesserThanFloat(f64),
    EqualsInt(i64),
    EqualsFloat(i64),
    DivisibleBy(i64),
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
enum BinaryAttributeRule {
    LongerThan(i64),
    ShorterThan(i64),
    StartsWith(String),
    EndsWith(String),
    Equals(String),
    Substring(usize, usize, String),
}

impl CollectAnalytics for &SplitQueuesConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        // TODO
    }
}
