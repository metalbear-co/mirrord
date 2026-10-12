//! Resolving a session's `feature.db_branches: "*"` / `["id", ...]` against the `dbBranches`
//! entries on the target's `MirrordSplitConfig`: the request the CLI posts to
//! `POST /apis/operator.metalbear.co/v1/splitconfigdbbranches` and the operator's answer.
//!
//! The CLI keeps creating the branches itself from the resolved entries; the operator only owns
//! the lookup, since it knows which `MirrordSplitConfig`s belong to a workload and how their
//! entries map to branch configs.

use serde::{Deserialize, Serialize};

use crate::crd::session::KubeResourceTarget;

/// Which `dbBranches` entries a session asks for. Same shape as the `feature.db_branches`
/// request forms in `mirrord.json`: `"*"` or a list of entry ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SplitConfigDbBranchesRequest {
    /// Every entry on the workload's `MirrordSplitConfig`s.
    All(AllEntries),
    /// The entries with these ids; an id matching no entry is an error.
    Ids(Vec<String>),
}

/// The literal `"*"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AllEntries {
    #[serde(rename = "*")]
    #[default]
    All,
}

/// Request body for `POST /splitconfigdbbranches`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolveSplitConfigDbBranchesRequest {
    /// Namespace of the target workload (where its `MirrordSplitConfig`s live).
    pub namespace: String,
    /// The target workload. The container is ignored.
    pub target: KubeResourceTarget,
    /// What to resolve. `None` means the session defines its branches inline: the operator
    /// resolves nothing and only reports, as warnings, the `MirrordSplitConfig`s whose
    /// `dbBranches` the session is ignoring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<SplitConfigDbBranchesRequest>,
}

/// Response of `POST /splitconfigdbbranches`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ResolveSplitConfigDbBranchesResponse {
    /// The resolved entries, in the order the operator merged them. Empty when the workload has
    /// no `MirrordSplitConfig` with `dbBranches`, or when `request` was `None`.
    #[serde(default)]
    pub entries: Vec<ResolvedSplitConfigDbBranch>,
    /// Names of the `MirrordSplitConfig`s the operator looked at, for messages.
    #[serde(default)]
    pub split_configs: Vec<String>,
    /// Warnings for the user, printed as they are.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// One resolved `dbBranches` entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedSplitConfigDbBranch {
    /// The entry id on the `MirrordSplitConfig`; the branch id is this plus the session key.
    pub id: String,
    /// The entry as a `feature.db_branches[]` config object (snake_case keys, `type` set),
    /// which the CLI reads with its own config version so a field this CLI does not know is
    /// reported instead of silently dropped.
    pub config: serde_json::Value,
}
