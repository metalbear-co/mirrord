use k8s_openapi::api::core::v1::Pod;
use kube::{api::ListParams, Api, Client, Resource, ResourceExt};
use mirrord_config::target::workflow::WorkflowTarget;

use crate::{
    api::{
        kubernetes::workflow::Workflow,
        runtime::{get_k8s_resource_api, RuntimeData, RuntimeDataProvider},
    },
    error::{KubeApiError, Result},
};

pub(crate) struct WorkflowRuntimeProvider<'a> {
    pub client: &'a Client,
    pub resource: &'a Workflow,
    pub template: Option<&'a str>,
    pub container: Option<&'a str>,
}

impl WorkflowRuntimeProvider<'_> {
    pub async fn try_into_runtime_data(self) -> Result<RuntimeData> {
        let entrypoint = match self.template.as_ref() {
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

        let (workflow_node_key, workflow_node) = self
            .resource
            .status
            .as_ref()
            .ok_or_else(|| KubeApiError::missing_field(self.resource, ".status"))?
            .nodes
            .iter()
            .find(|(_, node_status)| node_status.template_name.as_deref() == Some(entrypoint))
            .ok_or_else(|| {
                KubeApiError::missing_field(self.resource, format!(".status.nodes.{entrypoint}*"))
            })?;

        if workflow_node.r#type.as_deref() != Some("Pod") {
            let kind = Workflow::kind(&());
            let name = self.resource.meta().name.as_ref();
            let namespace = self
                .resource
                .meta()
                .namespace
                .as_deref()
                .unwrap_or("default");

            let message = match name {
                Some(name) => format!("{kind} `{namespace}/{name}` expected workflow node type to be \"Pod\" but got {:?}", workflow_node.r#type),
                None => format!("{kind} expected workflow node type to be \"Pod\" but got {:?}", workflow_node.r#type),
            };

            return Err(KubeApiError::MalformedResource(message));
        }

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
            container: self.container.as_deref(),
        }
        .try_into_runtime_data()
        .await
    }
}
