#![deny(unused_crate_dependencies)]

use std::{collections::HashMap, str::FromStr, time::Instant};

use base64::{Engine as _, engine::general_purpose};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{Level, info};
use uuid::Uuid;

pub mod preview;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Environment variable carrying the `mirrord up` correlation id down to child
/// mirrord sessions.
pub const MIRRORD_UP_CORRELATION_ID_ENV: &str = "MIRRORD_UP_CORRELATION_ID";

/// Reads the [`MIRRORD_UP_CORRELATION_ID_ENV`] correlation id, if this process
/// was spawned by `mirrord up`.
pub fn read_correlation_id_from_env() -> Option<Uuid> {
    std::env::var(MIRRORD_UP_CORRELATION_ID_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
}

/// Environment variables carrying the connected cluster's Kubernetes
/// apiserver version from the CLI down to the proxy that reports
/// session analytics.
///
/// Intproxy (or extproxy) is the main component responsible for
/// reporting analytics, but it does not have a kube client. CLI does
/// have a kube client, so it serializes the version into env for the
/// (int/ext)proxy to report.
pub const MIRRORD_KUBE_VERSION_MAJOR_ENV: &str = "MIRRORD_KUBE_VERSION_MAJOR";
pub const MIRRORD_KUBE_VERSION_MINOR_ENV: &str = "MIRRORD_KUBE_VERSION_MINOR";

/// Reads the Kubernetes apiserver version `(major, minor)` set by the
/// parent CLI, if present.
pub fn read_kube_version_from_env() -> Option<(u16, u16)> {
    let major = std::env::var(MIRRORD_KUBE_VERSION_MAJOR_ENV)
        .ok()?
        .parse()
        .ok()?;
    let minor = std::env::var(MIRRORD_KUBE_VERSION_MINOR_ENV)
        .ok()?
        .parse()
        .ok()?;
    Some((major, minor))
}

/// Possible values for analytic data
/// This is strict so we won't send sensitive data by accident.
/// (Don't add strings)
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum AnalyticValue {
    Bool(bool),
    Number(u32),
    Uuid(Uuid),
    Nested(Analytics),
    Hash(AnalyticsHash),
    List(Vec<AnalyticValue>),
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
    Dump = 4,
    Wizard = 5,
    Preview = 6,
    Other = 0,
}

impl From<u32> for ExecutionKind {
    fn from(kind: u32) -> Self {
        match kind {
            1 => ExecutionKind::Container,
            2 => ExecutionKind::Exec,
            3 => ExecutionKind::PortForward,
            4 => ExecutionKind::Dump,
            5 => ExecutionKind::Wizard,
            6 => ExecutionKind::Preview,
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
/// analytics.add("c", 3usize);
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

    /// Deterministically hashes a session key with the operator license fingerprint.
    pub fn for_session_key(key: &str, license_fingerprint: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(license_fingerprint.as_bytes());
        hasher.update(key.as_bytes());
        Self::from_bytes(&hasher.finalize())
    }

    pub fn as_str(&self) -> &str {
        &self.0
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

impl From<u16> for AnalyticValue {
    fn from(n: u16) -> Self {
        AnalyticValue::Number(n.into())
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

impl From<Uuid> for AnalyticValue {
    fn from(id: Uuid) -> Self {
        AnalyticValue::Uuid(id)
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

impl From<Vec<AnalyticValue>> for AnalyticValue {
    fn from(vec: Vec<AnalyticValue>) -> Self {
        AnalyticValue::List(vec)
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

#[derive(Debug, Clone, Copy)]
pub enum ReportTarget {
    ClientSession,
    UpSession,
}

/// Header the client sets to tell the analytics-server which event a report is.
/// The server maps known values to PostHog event names and treats anything
/// unrecognized (or missing) as a client session.
pub const EVENT_KIND_HEADER: &str = "x-mirrord-event-kind";

impl ReportTarget {
    /// The [`EVENT_KIND_HEADER`] value sent for this target.
    fn event_kind(self) -> &'static str {
        match self {
            ReportTarget::ClientSession => "client-session",
            ReportTarget::UpSession => "up-session",
        }
    }
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
    target: ReportTarget,
    session_key: Option<String>,
}

impl AnalyticsReporter {
    pub fn new(
        enabled: bool,
        execution_kind: ExecutionKind,
        watch: drain::Watch,
        machine_id: Uuid,
        session_key: Option<String>,
    ) -> Self {
        let mut analytics = Analytics::default();
        analytics.add("execution_kind", execution_kind as u32);
        analytics.add("machine_id", machine_id);
        analytics.add("is_ci", ci_info::is_ci());

        AnalyticsReporter {
            analytics,
            error_only_send: false,
            enabled,
            error: None,
            operator_properties: None,
            start_instant: Instant::now(),
            watch,
            target: ReportTarget::ClientSession,
            session_key,
        }
    }

    pub fn only_error(
        enabled: bool,
        execution_kind: ExecutionKind,
        watch: drain::Watch,
        machine_id: Uuid,
        session_key: Option<String>,
    ) -> Self {
        let mut reporter =
            AnalyticsReporter::new(enabled, execution_kind, watch, machine_id, session_key);
        reporter.error_only_send = true;
        reporter
    }

    /// Constructs a reporter that delivers to [`ReportTarget::UpSession`].
    pub fn for_up_event(enabled: bool, watch: drain::Watch, machine_id: Uuid) -> Self {
        let mut analytics = Analytics::default();
        analytics.add("machine_id", machine_id);
        analytics.add("is_ci", ci_info::is_ci());

        AnalyticsReporter {
            analytics,
            error_only_send: false,
            enabled,
            error: None,
            operator_properties: None,
            start_instant: Instant::now(),
            watch,
            target: ReportTarget::UpSession,
            session_key: None,
        }
    }

    fn as_report(&mut self) -> AnalyticsReport {
        let duration = self
            .start_instant
            .elapsed()
            .as_secs()
            .try_into()
            .unwrap_or(u32::MAX);

        if let Some(key) = self.session_key.as_deref()
            && let Some(license_fingerprint) = self
                .operator_properties
                .as_ref()
                .and_then(|properties| properties.license_hash.as_ref())
        {
            let session_key_identifier =
                AnalyticsHash::for_session_key(key, license_fingerprint.as_str());
            self.analytics
                .add("session_key_identifier", session_key_identifier);
        }

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
            let target = self.target;
            tokio::spawn(async move {
                send_analytics(report, target).await;
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

const ANALYTICS_ENDPOINT: &str = "https://analytics.metalbear.com/api/v1/event";

/// Actualy send `Analytics` & `AnalyticsOperatorProperties` to analytics.metalbear.com
#[tracing::instrument(level = Level::TRACE)]
async fn send_analytics(report: AnalyticsReport, target: ReportTarget) {
    let client = reqwest::Client::new();
    let res = client
        .post(ANALYTICS_ENDPOINT)
        .header(EVENT_KIND_HEADER, target.event_kind())
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

    #[test]
    fn hash_value_serialization() {
        let mut analytics = Analytics::default();
        analytics.add("preview_key_identifier", AnalyticsHash::from_bytes(b"key"));

        assert_json_eq!(
            analytics,
            json!({
                "preview_key_identifier": "a2V5"
            })
        );
    }
}
