use std::collections::BTreeMap;

use k8s_openapi::{
    api::core::v1::Container, apimachinery::pkg::apis::meta::v1::ObjectMeta, ListableResource,
    Metadata, NamespaceResourceScope, Resource,
};
use serde::{Deserialize, Serialize};

pub mod serialization;

#[derive(Clone, Debug)]
pub struct Workflow {
    pub metadata: ObjectMeta,
    pub spec: Option<WorkflowSpec>,
    pub status: Option<WorkflowStatus>,
}

impl Resource for Workflow {
    const API_VERSION: &'static str = "argoproj.io/v1alpha1";
    const GROUP: &'static str = "argoproj.io";
    const KIND: &'static str = "Workflow";
    const VERSION: &'static str = "v1alpha1";
    const URL_PATH_SEGMENT: &'static str = "workflows";
    type Scope = NamespaceResourceScope;
}

impl ListableResource for Workflow {
    const LIST_KIND: &'static str = "WorkflowList";
}

impl Metadata for Workflow {
    type Ty = ObjectMeta;

    fn metadata(&self) -> &Self::Ty {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut Self::Ty {
        &mut self.metadata
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSpec {
    pub entrypoint: Option<String>,

    pub templates: Vec<WorkflowTemplate>,

    #[serde(flatten)]
    _rest: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowTemplate {
    pub name: Option<String>,

    pub container: Option<Container>,

    #[serde(flatten)]
    _rest: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStatus {
    pub phase: Option<String>,

    #[serde(default)]
    pub nodes: BTreeMap<String, WorkflowNodeStatus>,

    #[serde(flatten)]
    _rest: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowNodeStatus {
    pub id: Option<String>,

    pub name: Option<String>,

    pub r#type: Option<String>,

    pub phase: Option<String>,

    pub template_name: Option<String>,

    #[serde(flatten)]
    _rest: serde_json::Value,
}
