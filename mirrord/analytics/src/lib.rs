use std::collections::HashMap;

use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use tracing::info;

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
    Hash(AnalyticsHash),
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

#[derive(Debug, Serialize, Deserialize)]
pub struct AnalyticsHash(#[serde(with = "serde_base64")] Vec<u8>);

impl AnalyticsHash {
    pub fn from_digest(val: u64) -> Self {
        AnalyticsHash(val.to_be_bytes().to_vec())
    }

    pub fn from_base64(val: &str) -> Option<Self> {
        general_purpose::STANDARD_NO_PAD
            .decode(val)
            .map(AnalyticsHash)
            .ok()
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

impl From<AnalyticsHash> for AnalyticValue {
    fn from(hash: AnalyticsHash) -> Self {
        AnalyticValue::Hash(hash)
    }
}

impl<T: CollectAnalytics> From<T> for AnalyticValue {
    fn from(other: T) -> Self {
        let mut analytics = Analytics::default();
        other.collect_analytics(&mut analytics);
        analytics.into()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct AnalyticsReport {
    event_properties: Analytics,
    platform: String,
    duration: u32,
    version: String,
    operator: bool,
}

pub async fn send_analytics(analytics: Analytics, duration: u32, operator: bool) {
    let report = AnalyticsReport {
        event_properties: analytics,
        platform: std::env::consts::OS.to_string(),
        version: CURRENT_VERSION.to_string(),
        duration,
        operator,
    };

    let client = reqwest::Client::new();
    let res = client
        .post("https://analytics.metalbear.co/api/v1/event")
        .json(&report)
        .send()
        .await;
    if let Err(e) = res {
        info!("Failed to send analytics: {e}");
    }
}

mod serde_base64 {
    use base64::{engine::general_purpose, Engine as _};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        let base64 = general_purpose::STANDARD_NO_PAD.encode(v);
        String::serialize(&base64, s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let base64 = String::deserialize(d)?;
        general_purpose::STANDARD_NO_PAD
            .decode(base64.as_bytes())
            .map_err(serde::de::Error::custom)
    }
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
