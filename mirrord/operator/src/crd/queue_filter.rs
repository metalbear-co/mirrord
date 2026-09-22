//! The queue message filter as it travels between the CLI and the operator, and as the operator
//! stores it on split sessions.
//!
//! Users write one of two shapes in `feature.split_queues`: the legacy `message_filter` map
//! (exact attribute name to a regex on its value) or the composable `filter` tree (`metadata`
//! regexes combined with `any_of` / `all_of`). Both lower into this one tree, so everything
//! past the wire - brokers, policies, status views - deals with a single type, and the legacy
//! map keeps its exact meaning through the [`MessageFilter::Attribute`] leaf instead of a lossy
//! rewrite into a `metadata` regex.

use std::{collections::BTreeMap, fmt};

use mirrord_config::feature::split_queues::{InnerMessageFilter, MessageFilterConfig};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A tree of regex leaves combined with `allOf` / `anyOf`.
///
/// Internally tagged so a stored tree stays readable when a variant is added: an operator that
/// does not know a node reads it as [`MessageFilter::Unknown`] and matches nothing, which is the
/// safe direction for a filter that decides what leaves the deployed application.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MessageFilter {
    /// The message has an attribute named exactly `key` whose value matches `pattern`. This is
    /// what one entry of the legacy `message_filter` map means.
    Attribute { key: String, pattern: String },

    /// Some attribute of the message, rendered as `<name>: <value>`, matches `pattern`.
    Metadata { pattern: String },

    /// Every filter matches. An empty list matches nothing, so a filter can never widen by
    /// accident.
    AllOf { filters: Vec<MessageFilter> },

    /// At least one filter matches. An empty list matches nothing.
    AnyOf { filters: Vec<MessageFilter> },

    /// A node written by a newer version. Matches nothing.
    #[schemars(skip)]
    #[serde(other)]
    Unknown,
}

impl MessageFilter {
    /// Every regex pattern in the tree, for validation and for policy checks that inspect the
    /// user's patterns as plain strings.
    pub fn patterns(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.collect_patterns(&mut out);
        out
    }

    fn collect_patterns<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Self::Attribute { pattern, .. } | Self::Metadata { pattern } => out.push(pattern),
            Self::AllOf { filters } | Self::AnyOf { filters } => {
                filters.iter().for_each(|f| f.collect_patterns(out))
            }
            Self::Unknown => {}
        }
    }

    /// Whether this tree is exactly what a legacy `message_filter` map lowers to, so status views
    /// and older shims can show it in the map shape again without losing anything.
    pub fn as_attribute_map(&self) -> Option<BTreeMap<String, String>> {
        let attribute = |filter: &Self| match filter {
            Self::Attribute { key, pattern } => Some((key.clone(), pattern.clone())),
            _ => None,
        };
        match self {
            Self::Attribute { .. } => attribute(self).map(|entry| [entry].into()),
            Self::AllOf { filters } => filters.iter().map(attribute).collect(),
            _ => None,
        }
    }
}

/// The legacy map is an `allOf` over exact attribute names.
impl From<&BTreeMap<String, String>> for MessageFilter {
    fn from(message_filter: &BTreeMap<String, String>) -> Self {
        Self::AllOf {
            filters: message_filter
                .iter()
                .map(|(key, pattern)| Self::Attribute {
                    key: key.clone(),
                    pattern: pattern.clone(),
                })
                .collect(),
        }
    }
}

impl From<&MessageFilterConfig> for MessageFilter {
    fn from(config: &MessageFilterConfig) -> Self {
        match config {
            MessageFilterConfig::Metadata { metadata } => Self::Metadata {
                pattern: metadata.clone(),
            },
            MessageFilterConfig::AllOf { all_of } => Self::AllOf {
                filters: all_of.iter().map(Self::from).collect(),
            },
            MessageFilterConfig::AnyOf { any_of } => Self::AnyOf {
                filters: any_of.iter().map(Self::from).collect(),
            },
        }
    }
}

impl From<&InnerMessageFilter> for MessageFilter {
    fn from(config: &InnerMessageFilter) -> Self {
        match config {
            InnerMessageFilter::Metadata { metadata } => Self::Metadata {
                pattern: metadata.clone(),
            },
        }
    }
}

/// Renders the tree the way `mirrord queues` and the TUI show it, e.g.
/// `any of (baggage: .*mirrord-session=abc.*), (all of (tenant=^acme$), (region: eu))`.
impl fmt::Display for MessageFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Attribute { key, pattern } => write!(f, "{key}={pattern}"),
            Self::Metadata { pattern } => f.write_str(pattern),
            Self::AllOf { filters } | Self::AnyOf { filters } => {
                let word = match self {
                    Self::AllOf { .. } => "all of ",
                    _ => "any of ",
                };
                f.write_str(word)?;
                for (index, filter) in filters.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "({filter})")?;
                }
                Ok(())
            }
            Self::Unknown => f.write_str("<unknown filter>"),
        }
    }
}

/// Schema for a [`MessageFilter`] field on a stored CRD.
///
/// The tree is recursive, and a Kubernetes structural schema cannot reference itself, so the
/// field is stored opaquely: the API server keeps whatever the client sent and the operator
/// validates it when it reads the resource. Use with `#[schemars(schema_with = ...)]`.
pub fn message_filter_crd_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut schema = schemars::json_schema!({ "type": "object", "nullable": true });
    schema.insert(
        "x-kubernetes-preserve-unknown-fields".to_owned(),
        serde_json::Value::Bool(true),
    );
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// A legacy map means "every named attribute matches its pattern", and lowering must keep the
    /// names exact rather than rewriting them into a metadata regex, so the round trip back to a
    /// map is lossless.
    #[test]
    fn legacy_map_lowers_to_all_of_attributes_and_back() {
        let legacy = map(&[("tenant", "^acme$"), ("type", "^premium$")]);

        let filter = MessageFilter::from(&legacy);
        assert_eq!(
            filter,
            MessageFilter::AllOf {
                filters: vec![
                    MessageFilter::Attribute {
                        key: "tenant".to_owned(),
                        pattern: "^acme$".to_owned()
                    },
                    MessageFilter::Attribute {
                        key: "type".to_owned(),
                        pattern: "^premium$".to_owned()
                    },
                ]
            }
        );
        assert_eq!(filter.as_attribute_map(), Some(legacy));
    }

    #[test]
    fn composed_tree_has_no_attribute_map() {
        let filter = MessageFilter::from(&MessageFilterConfig::AnyOf {
            any_of: vec![InnerMessageFilter::Metadata {
                metadata: "^baggage: .*$".to_owned(),
            }],
        });

        assert_eq!(filter.as_attribute_map(), None);
        assert_eq!(filter.patterns(), ["^baggage: .*$"]);
    }

    /// The stored shape is a contract: a renamed tag or variant would make every stored split
    /// session unreadable after an upgrade.
    #[test]
    fn wire_shape_is_stable() {
        let filter = MessageFilter::AnyOf {
            filters: vec![
                MessageFilter::Metadata {
                    pattern: "^baggage: .*$".to_owned(),
                },
                MessageFilter::AllOf {
                    filters: vec![MessageFilter::Attribute {
                        key: "tenant".to_owned(),
                        pattern: "^acme$".to_owned(),
                    }],
                },
            ],
        };

        assert_eq!(
            serde_json::to_value(&filter).unwrap(),
            serde_json::json!({
                "type": "anyOf",
                "filters": [
                    { "type": "metadata", "pattern": "^baggage: .*$" },
                    { "type": "allOf", "filters": [
                        { "type": "attribute", "key": "tenant", "pattern": "^acme$" }
                    ] }
                ]
            })
        );
        assert_eq!(
            filter.to_string(),
            "any of (^baggage: .*$), (all of (tenant=^acme$))"
        );
    }

    #[test]
    fn unknown_node_reads_as_unknown() {
        let filter: MessageFilter =
            serde_json::from_value(serde_json::json!({ "type": "regexOnBody", "pattern": "x" }))
                .unwrap();
        assert_eq!(filter, MessageFilter::Unknown);
    }
}
