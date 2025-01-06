#![deny(unused_crate_dependencies)]

use std::{collections::HashMap, str::FromStr, time::Instant};

use base64::{engine::general_purpose, Engine as _};
use serde::{Deserialize, Serialize};
use tracing::info;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Possible values for analytic data
/// This is strict so we won't send sensitive data by accident.
/// (Don't add strings)
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum AnalyticValue {
    Bool(bool),
    Number(u32),
    Nested(Analytics),
}

#[derive(Default, Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsError {
    AgentConnection,
    EnvFetch,
    BinaryExecuteFailed,
    IntProxyFirstConnection,

    #[default]
    Unknown,
}

#[derive(Default, Debug, Clone, Copy)]
#[repr(u32)]
pub enum ExecutionKind {
    Container = 1,
    #[default]
    Exec = 2,
    PortForward = 3,
    Other = 0,
}

impl From<u32> for ExecutionKind {
    fn from(kind: u32) -> Self {
        match kind {
            1 => ExecutionKind::Container,
            2 => ExecutionKind::Exec,
            3 => ExecutionKind::PortForward,
            _ => ExecutionKind::Other,
        }
    }
}

impl FromStr for ExecutionKind {
    type Err = <u32 as FromStr>::Err;

    fn from_str(value: &str) -> std::result::Result<Self, <Self as std::str::FromStr>::Err> {
        value.parse::<u32>().map(ExecutionKind::from)
    }
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
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
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
#[derive(Debug, Serialize, Deserialize, Clone)]
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

impl From<u64> for AnalyticValue {
    fn from(n: u64) -> Self {
        AnalyticValue::Number(u32::try_from(n).unwrap_or(u32::MAX))
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

pub trait Reporter: Sized {
    fn get_mut(&mut self) -> &mut Analytics;

    fn set_operator_properties(&mut self, operator_properties: AnalyticsOperatorProperties);

    fn set_error(&mut self, error: AnalyticsError);

    fn has_error(&self) -> bool;
}

/// Due to the drop nature using tokio::spawn, runtime must be started.
#[derive(Debug)]
pub struct AnalyticsReporter {
    pub enabled: bool,
    error_only_send: bool,
    analytics: Analytics,
    error: Option<AnalyticsError>,
    start_instant: Instant,
    operator_properties: Option<AnalyticsOperatorProperties>,
    watch: drain::Watch,
}

impl AnalyticsReporter {
    pub fn new(enabled: bool, execution_kind: ExecutionKind, watch: drain::Watch) -> Self {
        let mut analytics = Analytics::default();
        analytics.add("execution_kind", execution_kind as u32);

        AnalyticsReporter {
            analytics,
            error_only_send: false,
            enabled,
            error: None,
            operator_properties: None,
            start_instant: Instant::now(),
            watch,
        }
    }

    pub fn only_error(enabled: bool, execution_kind: ExecutionKind, watch: drain::Watch) -> Self {
        let mut reporter = AnalyticsReporter::new(enabled, execution_kind, watch);
        reporter.error_only_send = true;
        reporter
    }

    fn as_report(&self) -> AnalyticsReport {
        let duration = self
            .start_instant
            .elapsed()
            .as_secs()
            .try_into()
            .unwrap_or(u32::MAX);

        AnalyticsReport {
            duration,
            error: self.error,
            event_properties: self.analytics.clone(),
            operator: self.operator_properties.is_some(),
            operator_properties: self.operator_properties.clone(),
            platform: std::env::consts::OS,
            version: CURRENT_VERSION,
        }
    }
}

impl Reporter for AnalyticsReporter {
    fn get_mut(&mut self) -> &mut Analytics {
        &mut self.analytics
    }

    fn set_operator_properties(&mut self, operator_properties: AnalyticsOperatorProperties) {
        self.operator_properties.replace(operator_properties);
    }

    fn set_error(&mut self, error: AnalyticsError) {
        self.error.replace(error);
    }

    fn has_error(&self) -> bool {
        self.error.is_some()
    }
}

#[derive(Debug, Default)]
pub struct NullReporter {
    analytics: Analytics,
}

impl Reporter for NullReporter {
    fn get_mut(&mut self) -> &mut Analytics {
        &mut self.analytics
    }

    fn set_operator_properties(&mut self, _operator_properties: AnalyticsOperatorProperties) {}

    fn set_error(&mut self, _error: AnalyticsError) {}

    fn has_error(&self) -> bool {
        false
    }
}

/// Must be called in tokio runtime
/// We rely on the main tokio runtime to be started using the macro,
/// meaning it will wait for all ongoing tasks to finish before exiting.
impl Drop for AnalyticsReporter {
    fn drop(&mut self) {
        if self.enabled && (self.error.is_some() || !self.error_only_send) {
            let report = self.as_report();
            let watch = self.watch.clone();
            tokio::spawn(async move {
                send_analytics(report).await;
                // hold clone of watch to prevent it from being dropped
                // allowing our task to finish
                drop(watch);
            });
        }
    }
}

/// Extra fields for `AnalyticsReport` when using mirrord with operator.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AnalyticsOperatorProperties {
    /// client certificate public key
    pub client_hash: Option<AnalyticsHash>,

    /// sha256 fingerprint from operator license
    pub license_hash: Option<AnalyticsHash>,
}

#[derive(Debug, Serialize)]
struct AnalyticsReport {
    event_properties: Analytics,
    platform: &'static str,
    duration: u32,
    version: &'static str,
    operator: bool,
    #[serde(flatten)]
    operator_properties: Option<AnalyticsOperatorProperties>,
    error: Option<AnalyticsError>,
}

/// Actualy send `Analytics` & `AnalyticsOperatorProperties` to analytics.metalbear.co
#[tracing::instrument(level = "trace")]
async fn send_analytics(report: AnalyticsReport) {
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
