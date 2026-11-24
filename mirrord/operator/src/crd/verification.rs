//! CRD types for verification records.
//!
//! `mirrord verify run` creates a [`MirrordVerification`] before it runs a test command, and runs
//! the command with the record's name as the session key. The operator writes every session it
//! observes under that key, and the traffic it routes to those sessions, into the record's status,
//! and decides the record's [`VerificationPhase`] once the CLI has reported how the command ended.

use std::{fmt, ops::Not};

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::session::SessionOwner;

/// Prefix of the session key, and of the record name, of every verification.
pub const VERIFICATION_KEY_PREFIX: &str = "verify-";

/// Label holding the full commit hash a record is about.
pub const VERIFICATION_COMMIT_LABEL: &str = "verification.mirrord.metalbear.co/commit";

/// `payloadType` of the envelope that carries a record's signed statement.
pub const ATTESTATION_PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";

/// `predicateType` of a record's signed statement.
pub const ATTESTATION_PREDICATE_TYPE: &str = "https://mirrord.dev/attestation/verification/v0.1";

/// Number of leading commit hash characters in a verification key.
const KEY_COMMIT_LEN: usize = 12;

/// The session key of a verification of `commit`. `suffix` tells runs of one commit apart.
pub fn verification_key(commit: &str, suffix: &str) -> String {
    let commit = commit.get(..KEY_COMMIT_LEN).unwrap_or(commit);
    format!("{VERIFICATION_KEY_PREFIX}{commit}-{suffix}")
}

/// A test run against the cluster, as the CLI declared it and as the operator observed it.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[kube(
    group = "verification.mirrord.metalbear.co",
    group_resolver = "crate::types::keyed_group",
    version = "v1alpha",
    kind = "MirrordVerification",
    category = "mirrord",
    root = "MirrordVerification",
    status = "MirrordVerificationStatus",
    printcolumn = r#"{"name":"Phase", "type":"string", "description":"Verdict of the verification.", "jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Commit", "type":"string", "description":"Commit the verification is about.", "jsonPath":".spec.commit"}"#,
    printcolumn = r#"{"name":"Age", "type":"date", "description":"Time since the resource was created.", "jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordVerificationSpec {
    /// URL of the repository's `origin` remote, without credentials.
    pub repository: String,

    /// Full hash of the commit that was checked out when the command ran.
    pub commit: String,

    /// Hash of the commit's tree.
    pub tree: String,

    /// Whether the working tree had changes that the commit does not contain.
    pub dirty: bool,

    /// Branch that was checked out, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    /// The test command, as the user typed it.
    pub command: String,

    /// Version of the CLI that created the record.
    pub client_cli_version: String,

    /// How the command ended. The CLI adds it when the command exits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_report: Option<VerificationClientReport>,

    /// Traffic the operator must have routed to a session of the run. A record with checks
    /// passes only when the operator saw every one of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<VerificationCheck>,
}

/// Traffic that the operator must have routed to a session of the run, in the terms of the
/// events it reports to `mirrord subscribe`.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerificationCheck {
    /// Name shown in the record.
    pub name: String,

    /// The workload the traffic was addressed to. Any workload when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,

    /// An HTTP request that a session stole, and the session's answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpCheck>,

    /// Queue messages that a session received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<QueueCheck>,
}

/// An HTTP request that a session must have stolen and answered.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HttpCheck {
    /// Method of the request. Any method when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,

    /// Path of the request, without the query. Any path when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Status the session's answer must have. Any status when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

/// Queue messages that a session must have received.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QueueCheck {
    /// Name of the queue, topic or channel. Any when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// How many messages a session must have received. 1 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_least: Option<u64>,
}

/// How the test command ended, as reported by the CLI that ran it.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerificationClientReport {
    /// Exit code of the command.
    pub exit_code: i32,

    /// When the command exited, in RFC 3339 format.
    pub finished_at: String,
}

/// What the operator observed for a [`MirrordVerification`]. Only the operator writes it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MirrordVerificationStatus {
    /// Sessions the operator served under the verification's key.
    #[serde(default)]
    pub sessions: Vec<VerifiedSession>,

    /// The operator's verdict.
    #[serde(default)]
    pub phase: VerificationPhase,

    /// Traffic the operator routed to the sessions of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic: Option<WitnessedTraffic>,

    /// Whether the operator saw each check of the spec, in the same order. Set once the command
    /// has ended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckResult>,

    /// The record as a signed statement: a DSSE envelope, as JSON, signed by the operator once
    /// the verdict is final.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation: Option<String>,
}

/// Traffic the operator routed to the sessions of a verification, counted from the events it
/// reports to `mirrord subscribe`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WitnessedTraffic {
    /// HTTP requests, grouped by workload, method, path and answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub http: Vec<WitnessedHttp>,

    /// Queue messages, grouped by workload and queue.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queues: Vec<WitnessedQueue>,

    /// Events the operator dropped before it counted them, which makes the counts a lower bound.
    #[serde(default)]
    pub dropped_events: u64,

    /// Whether the operator saw more groups of traffic than it keeps, and left the rest out.
    #[serde(default)]
    pub truncated: bool,
}

impl WitnessedTraffic {
    /// Number of requests and messages in all groups.
    pub fn total(&self) -> u64 {
        let http = self.http.iter().map(|group| group.count).sum::<u64>();
        let queues = self.queues.iter().map(|group| group.count).sum::<u64>();
        http + queues
    }
}

/// HTTP requests that the operator routed to a session and that had the same answer.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WitnessedHttp {
    /// The workload the requests were addressed to.
    pub service: String,

    pub method: String,

    /// Path of the requests, without the query.
    pub path: String,

    /// Whether only the session received the requests. Otherwise the workload received them
    /// too, and the answer was the workload's.
    pub stolen: bool,

    /// Status of the answer. Absent when the operator saw no answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,

    pub count: u64,
}

/// Messages of one queue that the operator routed to a session.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WitnessedQueue {
    /// The workload that consumes the queue.
    pub service: String,

    /// The kind of queue, for example `sqs` or `kafka`.
    pub kind: String,

    /// Name of the queue, topic or channel.
    pub name: String,

    pub count: u64,
}

/// Whether the operator saw the traffic of one [`VerificationCheck`].
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    /// Name of the check.
    pub name: String,

    /// Whether the operator saw the traffic the check asks for.
    pub passed: bool,

    /// How many requests or messages matched the check.
    #[serde(default)]
    pub seen: u64,

    /// What the operator saw, in words.
    pub detail: String,
}

/// A session the operator served under a verification's key.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedSession {
    /// Name of the session resource.
    pub id: String,

    /// Who ran the session, as the operator authenticated them.
    pub owner: SessionOwner,

    /// Namespace of the session's target.
    pub namespace: String,

    /// The session's target, for example `Deployment/checkout`.
    pub target: String,

    /// When the session started, in RFC 3339 format.
    pub started_at: String,

    /// When the session ended, in RFC 3339 format. Absent while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
}

/// The operator's verdict on a [`MirrordVerification`].
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum VerificationPhase {
    /// The command has not reported how it ended.
    #[default]
    Running,

    /// The command succeeded on a clean checkout and the operator observed its sessions.
    Passed,

    /// The command failed.
    Failed,

    /// The command succeeded, but the checkout was dirty or the operator observed no session.
    Unwitnessed,

    /// A phase from an operator newer than this code.
    #[schemars(skip)]
    #[serde(other, skip_serializing)]
    Unknown,
}

impl fmt::Display for VerificationPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Running => "Running",
            Self::Passed => "Passed",
            Self::Failed => "Failed",
            Self::Unwitnessed => "Unwitnessed",
            Self::Unknown => "Unknown",
        };
        f.write_str(name)
    }
}

/// The verdict for a record with this spec, these observed sessions and these check results.
///
/// A record passes only when the command exited with 0 on a clean checkout, the operator observed
/// a session, and every check in the spec has a passing result.
pub fn decide_phase(
    spec: &MirrordVerificationSpec,
    sessions: &[VerifiedSession],
    checks: &[CheckResult],
) -> VerificationPhase {
    let Some(report) = &spec.client_report else {
        return VerificationPhase::Running;
    };

    if report.exit_code != 0 || checks.iter().any(|check| check.passed.not()) {
        VerificationPhase::Failed
    } else if spec.dirty || sessions.is_empty() || checks.len() < spec.checks.len() {
        VerificationPhase::Unwitnessed
    } else {
        VerificationPhase::Passed
    }
}

/// A DSSE envelope: a payload and signatures over its pre-authentication encoding ([`pae`]).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DsseEnvelope {
    pub payload_type: String,

    /// The payload, in standard base64.
    pub payload: String,

    pub signatures: Vec<DsseSignature>,
}

/// One signature of a [`DsseEnvelope`].
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DsseSignature {
    /// SHA-256 of the signer's public key, in hex.
    pub keyid: String,

    /// The signature, in standard base64.
    pub sig: String,
}

/// The bytes a DSSE signature covers, for a payload of the given type.
pub fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut encoded = format!(
        "DSSEv1 {} {payload_type} {} ",
        payload_type.len(),
        payload.len()
    )
    .into_bytes();
    encoded.extend_from_slice(payload);
    encoded
}

/// The in-toto statement the operator signs for a record: the commit as the subject, and the
/// record's spec and status as the predicate.
pub fn statement(
    name: &str,
    spec: &MirrordVerificationSpec,
    status: &MirrordVerificationStatus,
) -> Value {
    json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{
            "name": spec.repository,
            "digest": { "gitCommit": spec.commit },
        }],
        "predicateType": ATTESTATION_PREDICATE_TYPE,
        "predicate": {
            "record": name,
            "phase": status.phase,
            "tree": spec.tree,
            "dirty": spec.dirty,
            "command": spec.command,
            "clientReport": spec.client_report,
            "sessions": status.sessions,
            "traffic": status.traffic,
            "checks": status.checks,
        },
    })
}
