use fancy_regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Broker kind for a queue split entry in the new list form.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum QueueType {
    #[serde(rename = "SQS")]
    Sqs,
    #[serde(rename = "Kafka")]
    Kafka,
    #[serde(rename = "RMQ")]
    Rmq,
    #[serde(rename = "GCPPubSub")]
    GcpPubSub,
    #[serde(rename = "RedisPubSub")]
    RedisPubSub,
    #[serde(rename = "AzureServiceBus")]
    AzureServiceBus,
    #[serde(rename = "Temporal")]
    Temporal,
}

/// HTTP-style metadata filter for queue message splitting.
///
/// Regexes in `metadata` and composite inner rules run against each message metadata line formatted
/// as `"key: value"` (headers, SQS attributes, application properties, etc.).
#[derive(Clone, Debug, Eq, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueueMetadataFilterConfig {
    /// Match when any metadata line matches this regex.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,

    /// Jq program evaluated on each metadata line in `"key: value"` form; the line matches when
    /// the program outputs `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_jq: Option<String>,

    /// Jq program on the broker-specific JSON message representation (same role as legacy
    /// `jq_filter`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jq: Option<String>,

    /// Every inner rule must match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all_of: Option<Vec<QueueInnerFilter>>,

    /// At least one inner rule must match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub any_of: Option<Vec<QueueInnerFilter>>,
}

impl QueueMetadataFilterConfig {
    pub fn is_set(&self) -> bool {
        self.metadata.is_some()
            || self.metadata_jq.is_some()
            || self.jq.is_some()
            || self.all_of.as_ref().is_some_and(|v| !v.is_empty())
            || self.any_of.as_ref().is_some_and(|v| !v.is_empty())
    }

    pub fn verify(&self, queue_id: &str) -> Result<(), QueueMetadataFilterVerificationError> {
        let simple_fields = [
            self.metadata.as_deref(),
            self.metadata_jq.as_deref(),
            self.jq.as_deref(),
        ]
        .into_iter()
        .filter(|value| value.is_some())
        .count();

        let composite_fields = [self.all_of.as_ref(), self.any_of.as_ref()]
            .into_iter()
            .filter(|value| value.is_some_and(|filters| !filters.is_empty()))
            .count();

        if simple_fields == 0 && composite_fields == 0 {
            return Err(QueueMetadataFilterVerificationError::EmptyFilter(
                queue_id.to_owned(),
            ));
        }

        if simple_fields > 0 && composite_fields > 0 {
            return Err(QueueMetadataFilterVerificationError::MixedFilterStyles(
                queue_id.to_owned(),
            ));
        }

        if let Some(metadata) = &self.metadata {
            verify_regex(queue_id, "filter.metadata", metadata)?;
        }
        if let Some(metadata_jq) = &self.metadata_jq {
            verify_jq(queue_id, "filter.metadata_jq", metadata_jq)?;
        }
        if let Some(jq) = &self.jq {
            verify_jq(queue_id, "filter.jq", jq)?;
        }
        if let Some(filters) = &self.all_of {
            verify_inner_filters(queue_id, "filter.all_of", filters)?;
        }
        if let Some(filters) = &self.any_of {
            verify_inner_filters(queue_id, "filter.any_of", filters)?;
        }

        Ok(())
    }
}

#[derive(PartialEq, Eq, Clone, Debug, JsonSchema, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum QueueInnerFilter {
    Metadata { metadata: String },
    Jq { jq: String },
}

fn verify_inner_filters(
    queue_id: &str,
    field: &str,
    filters: &[QueueInnerFilter],
) -> Result<(), QueueMetadataFilterVerificationError> {
    if filters.is_empty() {
        return Err(QueueMetadataFilterVerificationError::EmptyCompositeList {
            queue_id: queue_id.to_owned(),
            field: field.to_owned(),
        });
    }

    for (index, filter) in filters.iter().enumerate() {
        match filter {
            QueueInnerFilter::Metadata { metadata } => {
                verify_regex(queue_id, &format!("{field}[{index}].metadata"), metadata)?;
            }
            QueueInnerFilter::Jq { jq } => {
                verify_jq(queue_id, &format!("{field}[{index}].jq"), jq)?;
            }
        }
    }
    Ok(())
}

fn verify_regex(
    queue_id: &str,
    field: &str,
    pattern: &str,
) -> Result<(), QueueMetadataFilterVerificationError> {
    if pattern.is_empty() {
        return Err(QueueMetadataFilterVerificationError::EmptyString {
            queue_id: queue_id.to_owned(),
            field: field.to_owned(),
        });
    }
    Regex::new(pattern).map_err(|error| QueueMetadataFilterVerificationError::InvalidRegex {
        queue_id: queue_id.to_owned(),
        field: field.to_owned(),
        error: error.into(),
    })?;
    Ok(())
}

fn verify_jq(
    queue_id: &str,
    field: &str,
    program: &str,
) -> Result<(), QueueMetadataFilterVerificationError> {
    if program.is_empty() {
        return Err(QueueMetadataFilterVerificationError::EmptyString {
            queue_id: queue_id.to_owned(),
            field: field.to_owned(),
        });
    }
    mirrord_jaq::compile_jq(program).map_err(|err| {
        QueueMetadataFilterVerificationError::InvalidJqProgram {
            queue_id: queue_id.to_owned(),
            field: field.to_owned(),
            jq_compile_errors: err.to_string(),
        }
    })?;
    Ok(())
}

#[derive(Error, Debug)]
pub enum QueueMetadataFilterVerificationError {
    #[error("{0}: filter must specify at least one rule")]
    EmptyFilter(String),
    #[error("{0}: cannot combine simple filter fields with any_of or all_of")]
    MixedFilterStyles(String),
    #[error("{queue_id}.{field}: filter string must not be empty")]
    EmptyString { queue_id: String, field: String },
    #[error("{queue_id}.{field}: composite filter list must not be empty")]
    EmptyCompositeList { queue_id: String, field: String },
    #[error("{queue_id}.{field}: failed to parse regular expression ({error})")]
    InvalidRegex {
        queue_id: String,
        field: String,
        error: Box<fancy_regex::Error>,
    },
    #[error("{queue_id}.{field}: invalid jq program:\n{jq_compile_errors}")]
    InvalidJqProgram {
        queue_id: String,
        field: String,
        jq_compile_errors: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_mixed_simple_and_composite() {
        let filter = QueueMetadataFilterConfig {
            metadata: Some(".*".to_owned()),
            any_of: Some(vec![QueueInnerFilter::Metadata {
                metadata: ".*".to_owned(),
            }]),
            ..Default::default()
        };
        assert!(matches!(
            filter.verify("q"),
            Err(QueueMetadataFilterVerificationError::MixedFilterStyles(_))
        ));
    }

    #[test]
    fn accepts_simple_metadata_filter() {
        let filter = QueueMetadataFilterConfig {
            metadata: Some(".*mirrord-session=.*".to_owned()),
            ..Default::default()
        };
        filter.verify("q").unwrap();
    }
}
