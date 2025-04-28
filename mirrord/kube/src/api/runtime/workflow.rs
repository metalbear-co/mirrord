use k8s_openapi::api::core::v1::Pod;
use kube::{api::ListParams, Api, Client, Resource, ResourceExt};
use mirrord_config::target::workflow::WorkflowTarget;

use crate::{
    api::{
        kubernetes::workflow::{Workflow, WorkflowNodeStatus},
        runtime::{get_k8s_resource_api, RuntimeData, RuntimeDataProvider},
    },
    error::{KubeApiError, Result},
};

pub(crate) struct WorkflowRuntimeProvider<'a> {
    pub client: &'a Client,
    pub resource: &'a Workflow,
    pub template: Option<&'a str>,
    pub step: Option<&'a str>,
    pub container: Option<&'a str>,
}

enum TargetLookup<'a> {
    Template(&'a str),
    Step(&'a str, String),
}

impl TargetLookup<'_> {
    fn matches(&self, status: &WorkflowNodeStatus) -> bool {
        status.r#type.as_deref() == Some("Pod")
            && match self {
                TargetLookup::Template(template_name) => {
                    status.template_name.as_deref() == Some(template_name)
                }
                TargetLookup::Step(prefix, suffix) => status
                    .name
                    .as_ref()
                    .map(|name| name.starts_with(prefix) && name.ends_with(suffix))
                    .unwrap_or(false),
            }
    }
}

fn workflow_lookup_error(resource: &Workflow) -> KubeApiError {
    let kind = Workflow::kind(&());
    let name = resource.meta().name.as_ref();
    let namespace = resource.meta().namespace.as_deref().unwrap_or("default");

    let message = match name {
        Some(name) => {
            format!("{kind} `{namespace}/{name}` unable to resolve requested target in workflow")
        }
        None => format!("{kind} unable to resolve requested target in workflow"),
    };

    KubeApiError::MalformedResource(message)
}

impl WorkflowRuntimeProvider<'_> {
    pub async fn try_into_runtime_data(self) -> Result<RuntimeData> {
        let template_name = match self.template.as_ref() {
            Some(template) => template,
            None => self
                .resource
                .spec
                .as_ref()
                .ok_or_else(|| KubeApiError::missing_field(self.resource, ".spec"))?
                .entrypoint
                .as_deref()
                .ok_or_else(|| KubeApiError::missing_field(self.resource, ".spec.entrypoint"))?,
        };

        let workflow_status = self
            .resource
            .status
            .as_ref()
            .ok_or_else(|| KubeApiError::missing_field(self.resource, ".status"))?;

        let lookup = match self.step.as_deref() {
            Some(step) => {
                let (steps_workflow_node_key, steps_workflow_node) = workflow_status
                    .nodes
                    .iter()
                    .find(|(_, node_status)| {
                        node_status.r#type.as_deref() == Some("Steps")
                            && node_status.template_name.as_deref() == Some(template_name)
                    })
                    .ok_or_else(|| workflow_lookup_error(&self.resource))?;

                let steps_workflow_node_id =
                    steps_workflow_node.id.as_deref().ok_or_else(|| {
                        KubeApiError::missing_field(
                            self.resource,
                            format!(".status.nodes.{steps_workflow_node_key}.id"),
                        )
                    })?;

                TargetLookup::Step(steps_workflow_node_id, format!(".{step}"))
            }
            None => TargetLookup::Template(template_name),
        };

        let (workflow_node_key, workflow_node) = workflow_status
            .nodes
            .iter()
            .find(|(_, node_status)| lookup.matches(node_status))
            .ok_or_else(|| workflow_lookup_error(&self.resource))?;

        let workflow_node_id = workflow_node.id.as_deref().ok_or_else(|| {
            KubeApiError::missing_field(
                self.resource,
                format!(".status.nodes.{workflow_node_key}.id"),
            )
        })?;

        let pod_api: Api<Pod> =
            get_k8s_resource_api(self.client, self.resource.meta().namespace.as_deref());

        let pods = pod_api
            .list(&ListParams {
                label_selector: Some(format!(
                    "workflows.argoproj.io/workflow={}",
                    self.resource.name_any() // name_any is fine because we fetched it by id.
                )),
                ..Default::default()
            })
            .await?;

        pods.items
            .iter()
            .filter(|pod| {
                pod.metadata
                    .annotations
                    .as_ref()
                    .and_then(|annotations| {
                        annotations
                            .get("workflows.argoproj.io/node-id")
                            .map(|node_id| node_id == workflow_node_id)
                    })
                    .unwrap_or(false)
            })
            .filter_map(|pod| {
                RuntimeData::from_pod(pod, self.container)
                    .inspect_err(|error| tracing::warn!(?error, "runtime data from pod info"))
                    .ok()
            })
            .next()
            .ok_or_else(|| {
                KubeApiError::invalid_state(
                    self.resource,
                    "no pod matching the labels is ready to be targeted",
                )
            })
    }
}

impl RuntimeDataProvider for WorkflowTarget {
    async fn runtime_data(&self, client: &Client, namespace: Option<&str>) -> Result<RuntimeData> {
        let api: Api<Workflow> = get_k8s_resource_api(client, namespace);
        let resource = api.get(&self.workflow).await?;

        WorkflowRuntimeProvider {
            client,
            resource: &resource,
            template: self.template.as_deref(),
            step: self.step.as_deref(),
            container: self.container.as_deref(),
        }
        .try_into_runtime_data()
        .await
    }
}
