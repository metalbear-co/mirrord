use std::{borrow::Cow, collections::BTreeMap};

use k8s_openapi::api::core::v1::Pod;
use kube::{Api, Client, Resource};
use mirrord_config::target::workflow::WorkflowTarget;

use super::RuntimeDataFromLabels;
use crate::{
    api::{kubernetes::workflow::Workflow, runtime::get_k8s_resource_api},
    error::{KubeApiError, Result},
};

impl RuntimeDataFromLabels for WorkflowTarget {
    type Resource = Workflow;

    fn name(&self) -> Cow<str> {
        Cow::from(&self.workflow)
    }

    fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }

    /// Digs into `resource` to return its `.spec.selector.matchLabels`.
    fn get_selector_match_labels(resource: &Self::Resource) -> Result<BTreeMap<String, String>> {
        let entrypoint = resource
            .spec
            .as_ref()
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec"))?
            .entrypoint
            .as_deref()
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.entrypoint"))?;

        todo!(
            "{entrypoint} -> {spec:#?} {status:#?}",
            spec = resource.spec,
            status = resource.status
        )
    }

    // Override auto implementaion because `LabelSelector` needs to be async fetched for Workflow
    async fn get_pods(resource: &Self::Resource, client: &Client) -> Result<Vec<Pod>> {
        let entrypoint = resource
            .spec
            .as_ref()
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec"))?
            .entrypoint
            .as_deref()
            .ok_or_else(|| KubeApiError::missing_field(resource, ".spec.entrypoint"))?;

        let (workflow_node_key, workflow_node) = resource
            .status
            .as_ref()
            .ok_or_else(|| KubeApiError::missing_field(resource, ".status"))?
            .nodes
            .iter()
            .find(|(_, node_status)| node_status.template_name.as_deref() == Some(entrypoint))
            .ok_or_else(|| {
                KubeApiError::missing_field(resource, format!(".status.nodes.{entrypoint}*"))
            })?;

        if workflow_node.r#type.as_deref() != Some("Pod") {
            let kind = Self::Resource::kind(&());
            let name = resource.meta().name.as_ref();
            let namespace = resource.meta().namespace.as_deref().unwrap_or("default");

            let message = match name {
                Some(name) => format!("{kind} `{namespace}/{name}` expected workflow node type to be \"Pod\" but got {:?}", workflow_node.r#type),
                None => format!("{kind} expected workflow node type to be \"Pod\" but got {:?}", workflow_node.r#type),
            };

            return Err(KubeApiError::MalformedResource(message));
        }

        let pod_name = workflow_node.name.as_deref().ok_or_else(|| {
            KubeApiError::missing_field(resource, format!(".status.nodes.{workflow_node_key}.name"))
        })?;

        tracing::warn!(?pod_name);

        let pod_api: Api<Pod> = get_k8s_resource_api(client, resource.meta().namespace.as_deref());
        let pod = pod_api.get(&pod_name).await?;

        Ok(vec![pod])
    }
}
