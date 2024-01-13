use std::collections::HashMap;

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;

/// Maps name of a `MirrordQueueSplitter` resource to a mapping from a fork name to a filter
/// definition.
/// ```json
/// {
///   "feature": {
///     "split_queues": {
///       "whatever-q-splitter": {
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
#[derive(Clone, Debug, MirrordConfig)]
struct SplitQueuesConfig(HashMap<String, HashMap<String, QueueMessageFilter>>);

enum QueueMessageFilter {
    SQS(HashMap<String, SQSAttributeRuleSet>),
}

/// Set of rules to filter that are required to apply to a single field
enum SQSAttributeRuleSet {
    /// A single regex is enough for complex rules for a string.
    StringAttribute(String),

    NumberAttribute(Vec<NumberRule>),

    BinaryAttribute(Vec<BinaryAttributeRule>),
}

enum NumberRule {
    GreaterThanInt(i64),
    LesserThanInt(i64),
    GreaterThanFloat(f64),
    LesserThanFloat(f64),
    EqualsInt(i64),
    EqualsFloat(i64),
    DivisibleBy(i64),
}

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
