use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{Analytics, AnalyticsHash, AnalyticsReporter, ExecutionKind, Reporter};

pub const PREVIEW_START_TYPE: &str = "preview_env_start";
pub const PREVIEW_STOP_TYPE: &str = "preview_env_stop";
pub const PREVIEW_FAILED_TYPE: &str = "preview_env_failed";
pub const PREVIEW_STATUS_TYPE: &str = "preview_env_status";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PreviewEvent {
    pub preview_key_identifier: AnalyticsHash,
    pub runtime_seconds: u32,
    #[serde(flatten)]
    pub kind: PreviewEventKind,
}

impl PreviewEvent {
    pub fn new(preview_key: &str, runtime_seconds: u32, kind: PreviewEventKind) -> Self {
        let preview_key_identifier =
            AnalyticsHash::from_bytes(&Sha256::digest(preview_key.as_bytes()));

        Self {
            preview_key_identifier,
            runtime_seconds,
            kind,
        }
    }

    pub fn event_type(&self) -> &'static str {
        match self.kind {
            PreviewEventKind::Start => PREVIEW_START_TYPE,
            PreviewEventKind::Stop => PREVIEW_STOP_TYPE,
            PreviewEventKind::Status => PREVIEW_STATUS_TYPE,
            PreviewEventKind::Failed { .. } => PREVIEW_FAILED_TYPE,
        }
    }

    /// Reports this event via the CLI analytics pipeline.
    pub fn report_analytics(&self, enabled: bool, watch: drain::Watch, machine_id: Uuid) {
        let mut reporter =
            AnalyticsReporter::new(enabled, ExecutionKind::Preview, watch, machine_id);

        let mut analytics = Analytics::default();

        analytics.add(
            "preview_key_identifier",
            self.preview_key_identifier.clone(),
        );
        analytics.add("runtime_seconds", self.runtime_seconds);

        reporter.get_mut().add(self.event_type(), analytics);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum PreviewEventKind {
    /// Emitted by the operator when a preview session task is spawned.
    Start,
    /// Emitted by the operator when a preview session stops and task ownership is removed.
    Stop,
    /// Emitted by the CLI for `mirrord preview status`.
    Status,
    /// Emitted by the operator when a preview session transitions to `Failed`.
    Failed { reason: String },
}
