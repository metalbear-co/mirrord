//! CRD types for verification records.
//!
//! `mirrord verify run` creates a [`MirrordVerification`] before it runs a test command, and runs
//! the command with the record's name as the session key. The operator writes every session it
//! observes under that key into the record's status, and decides the record's
//! [`VerificationPhase`] once the CLI has reported how the command ended.

use std::fmt;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::session::SessionOwner;

/// Prefix of the session key, and of the record name, of every verification.
pub const VERIFICATION_KEY_PREFIX: &str = "verify-";

/// Label holding the full commit hash a record is about.
pub const VERIFICATION_COMMIT_LABEL: &str = "verification.mirrord.metalbear.co/commit";

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

/// The verdict for a record with this spec and these observed sessions.
pub fn decide_phase(
    spec: &MirrordVerificationSpec,
    sessions: &[VerifiedSession],
) -> VerificationPhase {
    match &spec.client_report {
        None => VerificationPhase::Running,
        Some(report) if report.exit_code != 0 => VerificationPhase::Failed,
        Some(_) if spec.dirty || sessions.is_empty() => VerificationPhase::Unwitnessed,
        Some(_) => VerificationPhase::Passed,
    }
}
