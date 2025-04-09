use k8s_openapi::{
    api::core::v1::PodTemplateSpec, apimachinery::pkg::apis::meta::v1::LabelSelector,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RolloutSpec2 {
    pub replicas: Option<i32>,
    pub analysis: Option<AnalysisSpec>,
    pub selector: Option<LabelSelector>,
    pub workload_ref: Option<WorkloadRef2>,
    pub template: Option<PodTemplateSpec>,
    pub min_ready_seconds: Option<i32>,
    pub revision_history_limit: Option<i32>,
    pub paused: Option<bool>,
    pub progress_deadline_seconds: Option<i32>,
    pub progress_deadline_abort: Option<bool>,
    pub restart_at: Option<String>,
    pub rollback_window: Option<RollbackWindow>,
    pub strategy: Option<StrategySpec>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisSpec {
    pub successful_run_history_limit: Option<i32>,
    pub unsuccessful_run_history_limit: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadRef2 {
    pub api_version: String,
    pub kind: String,
    pub name: String,
    pub scale_down: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RollbackWindow {
    pub revisions: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StrategySpec {
    pub blue_green: Option<BlueGreenStrategy>,
    pub canary: Option<CanaryStrategy>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BlueGreenStrategy {
    pub active_service: String,
    pub preview_service: Option<String>,
    pub preview_replica_count: Option<i32>,
    pub auto_promotion_enabled: Option<bool>,
    pub auto_promotion_seconds: Option<i32>,
    pub scale_down_delay_seconds: Option<i32>,
    pub scale_down_delay_revision_limit: Option<i32>,
    pub abort_scale_down_delay_seconds: Option<i32>,
    pub active_metadata: Option<Metadata>,
    pub preview_metadata: Option<Metadata>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CanaryStrategy {
    pub canary_service: Option<String>,
    pub stable_service: Option<String>,
    pub canary_metadata: Option<Metadata>,
    pub stable_metadata: Option<Metadata>,
    pub max_unavailable: Option<i32>,
    pub max_surge: Option<String>,
    pub scale_down_delay_seconds: Option<i32>,
    pub min_pods_per_replica_set: Option<i32>,
    pub scale_down_delay_revision_limit: Option<i32>,
    pub analysis: Option<AnalysisSpec>,
    pub steps: Option<Vec<CanaryStep>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub labels: Option<std::collections::HashMap<String, String>>,
    pub annotations: Option<std::collections::HashMap<String, String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CanaryStep {
    pub set_weight: Option<i32>,
    pub pause: Option<Pause>,
    pub set_canary_scale: Option<CanaryScale>,
    pub plugin: Option<Plugin>,
    pub set_header_route: Option<HeaderRoute>,
    pub set_mirror_route: Option<MirrorRoute>,
    pub analysis: Option<AnalysisSpec>,
    pub experiment: Option<Experiment>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Pause {
    pub duration: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CanaryScale {
    pub replicas: Option<i32>,
    pub weight: Option<i32>,
    pub match_traffic_weight: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Plugin {
    pub name: String,
    pub config: Option<std::collections::HashMap<String, Value>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HeaderRoute {
    pub name: String,
    pub match_rules: Option<Vec<HeaderMatchRule>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderMatchRule {
    pub header_name: String,
    pub header_value: HeaderValue,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HeaderValue {
    Exact(String),
    Regex(String),
    Prefix(String),
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MirrorRoute {
    pub name: String,
    pub percentage: Option<i32>,
    pub match_rules: Option<Vec<MatchRule>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MatchRule {
    pub method: Option<MatchType>,
    pub path: Option<MatchType>,
    pub headers: Option<std::collections::HashMap<String, MatchType>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MatchType {
    Exact(String),
    Regex(String),
    Prefix(String),
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Experiment {
    pub duration: Option<String>,
    pub templates: Option<Vec<ExperimentTemplate>>,
    pub analyses: Option<Vec<AnalysisSpec>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentTemplate {
    pub name: String,
    pub spec_ref: String,
    pub service: Option<Service>,
    pub weight: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    pub name: Option<String>,
}
