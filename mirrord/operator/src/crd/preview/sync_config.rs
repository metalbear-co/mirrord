//! The code sync settings of a preview session.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Code sync settings of a preview session.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSyncConfig {
    /// The directories that the CLI pushes into the preview pod. A push names each entry by its
    /// index in this list.
    pub entries: Vec<PreviewSyncEntry>,
}

/// One directory that the CLI pushes into the preview pod.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSyncEntry {
    /// The absolute path in the target container that holds the pushed tree.
    pub remote: String,
}
