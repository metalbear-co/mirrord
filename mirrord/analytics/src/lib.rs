use std::{collections::HashMap, time::Instant};

use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
// use tracing::info;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Possible values for analytic data
/// This is strict so we won't send sensitive data by accident.
/// (Don't add strings)
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnalyticValue {
    Bool(bool),
    Number(u32),
    Nested(Analytics),
}

#[derive(Default, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticError {
    AgentConnection,

    #[default]
    Unknown,
}

/// Struct to store analytics data.
/// Example usage that would output the following json
/// ```json
/// {
///     "a": true,
///     "b": false,
///     "c": 3,
///     "extra": {
///         "d": true,
///         "e": true
///     }
/// }
/// ```
/// ```
/// use mirrord_analytics::{Analytics, CollectAnalytics};
/// let mut analytics = Analytics::default();
/// analytics.add("a", true);
/// analytics.add("b", false);
/// analytics.add("c", 3);
///
/// struct A {}
/// impl CollectAnalytics for A {
///     fn collect_analytics(&self, analytics: &mut Analytics) {
///         analytics.add("d", true);
///     }
/// }
///
/// struct B {}
/// impl CollectAnalytics for B {
///     fn collect_analytics(&self, analytics: &mut Analytics) {
///         let a = A {};
///         a.collect_analytics(analytics);
///         analytics.add("e", true);
///     }
/// }
/// let b = B {};
/// analytics.add("extra", b);
/// ```
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Analytics {
    #[serde(flatten)]
    data: HashMap<String, AnalyticValue>,
}

impl Analytics {
    pub fn add<Key: ToString, Value: Into<AnalyticValue>>(&mut self, key: Key, value: Value) {
        self.data.insert(key.to_string(), value.into());
    }
}

/// Type safe abstraction for Bytes to send hash values, should be explicitly created so we woun't
/// accidentaly send sensitive data
///
/// Saved as base64 for more optimal size of json
#[derive(Debug, Serialize, Deserialize)]
pub struct AnalyticsHash(String);

impl AnalyticsHash {
    /// Create AnalyticsHash from hash bytes
    pub fn from_bytes(bytes: &[u8]) -> Self {
        AnalyticsHash(general_purpose::STANDARD_NO_PAD.encode(bytes))
    }

    /// Create AnalyticsHash from base64 string
    pub fn from_base64(val: &str) -> Self {
        AnalyticsHash(val.to_owned())
    }
}

/// Structs that collect analytics about themselves should implement this trait
pub trait CollectAnalytics {
    /// Write analytics data to the given `Analytics` struct
    fn collect_analytics(&self, analytics: &mut Analytics);
}

impl From<bool> for AnalyticValue {
    fn from(b: bool) -> Self {
        AnalyticValue::Bool(b)
    }
}

impl From<u32> for AnalyticValue {
    fn from(n: u32) -> Self {
        AnalyticValue::Number(n)
    }
}

impl From<usize> for AnalyticValue {
    fn from(n: usize) -> Self {
        AnalyticValue::Number(u32::try_from(n).unwrap_or(u32::MAX))
    }
}

impl From<Analytics> for AnalyticValue {
    fn from(analytics: Analytics) -> Self {
        AnalyticValue::Nested(analytics)
    }
}

impl<T: CollectAnalytics> From<T> for AnalyticValue {
    fn from(other: T) -> Self {
        let mut analytics = Analytics::default();
        other.collect_analytics(&mut analytics);
        analytics.into()
    }
}

#[derive(Debug)]
pub struct AnalyticsReporter {
    pub enabled: bool,

    analytics: Analytics,
    error: Option<AnalyticError>,
    start_instant: Instant,
    operator_properties: Option<AnalyticsOperatorProperties>,
}

impl AnalyticsReporter {
    pub fn new(enabled: bool) -> Self {
        AnalyticsReporter {
            analytics: Analytics::default(),
            enabled,
            error: None,
            operator_properties: None,
            start_instant: Instant::now(),
        }
    }

    pub fn get_mut(&mut self) -> &mut Analytics {
        &mut self.analytics
    }

    pub fn set_operator_properties(&mut self, operator_properties: AnalyticsOperatorProperties) {
        self.operator_properties.replace(operator_properties);
    }

    pub fn set_error(&mut self, error: AnalyticError) {
        self.error.replace(error);
    }

    pub fn has_error(&self) -> bool {
        self.error.is_some()
    }

    fn into_report(self) -> AnalyticsReport {
        let duration = self
            .start_instant
            .elapsed()
            .as_secs()
            .try_into()
            .unwrap_or(u32::MAX);
        let platform = std::env::consts::OS.to_string();
        let version = CURRENT_VERSION.to_string();

        AnalyticsReport {
            duration,
            error: self.error,
            event_properties: self.analytics,
            operator: self.operator_properties.is_some(),
            operator_properties: self.operator_properties,
            platform,
            version,
        }
    }
}

/// Extra fields for `AnalyticsReport` when using mirrord with operator.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnalyticsOperatorProperties {
    /// sha256 fingerprint from client certificate
    pub client_hash: Option<AnalyticsHash>,

    /// sha256 fingerprint from operator license
    pub license_hash: Option<AnalyticsHash>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AnalyticsReport {
    event_properties: Analytics,
    platform: String,
    duration: u32,
    version: String,
    operator: bool,
    #[serde(flatten)]
    operator_properties: Option<AnalyticsOperatorProperties>,
    error: Option<AnalyticError>,
}

/// Actualy send `Analytics` & `AnalyticsOperatorProperties` to analytics.metalbear.co
#[tracing::instrument(level = "trace")]
pub async fn send_analytics(reporter: AnalyticsReporter) {
    if !reporter.enabled {
        return;
    }

    let report = reporter.into_report();

    let _ = std::fs::write("./result.json", serde_json::to_vec_pretty(&report).unwrap());

    // let client = reqwest::Client::new();
    // let res = client
    //     .post("https://analytics.metalbear.co/api/v1/event")
    //     .json(&report)
    //     .send()
    //     .await;
    // if let Err(e) = res {
    //     info!("Failed to send analytics: {e}");
    // }
}

#[cfg(test)]
mod tests {
    use assert_json_diff::assert_json_eq;
    use serde_json::json;

    use super::*;
    /// this tests creates a struct that is flatten and one that is nested
    /// serializes it and verifies it's correct
    #[test]
    fn happy_flow() {
        let mut analytics = Analytics::default();
        analytics.add("a", true);
        analytics.add("b", false);
        analytics.add("c", 3usize);

        struct A {}
        impl CollectAnalytics for A {
            fn collect_analytics(&self, analytics: &mut Analytics) {
                analytics.add("d", true);
            }
        }

        struct B {}
        impl CollectAnalytics for B {
            fn collect_analytics(&self, analytics: &mut Analytics) {
                let a = A {};
                a.collect_analytics(analytics);
                analytics.add("e", true);
            }
        }
        let b = B {};
        analytics.add("extra", b);

        assert_json_eq!(
            analytics,
            json!({
                "a": true,
                "b": false,
                "c": 3,
                "extra": {
                    "d": true,
                    "e": true
                }
            })
        );
    }
}
