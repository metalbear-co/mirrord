use std::{borrow::Cow, collections::BTreeMap};

use mirrord_config::target::workflow::WorkflowTarget;

use super::RuntimeDataFromLabels;
use crate::{api::kubernetes::workflow::Workflow, error::Result};

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
        todo!("{resource:#?}")
    }

    // // Override auto implementaion because `LabelSelector` needs to be async fetched for Workflow
    // async fn get_pods(resource: &Self::Resource, client: &Client) -> Result<Vec<Pod>> {
    //     let formatted_labels = resource
    //         .get_match_labels(client)
    //         .await?
    //         .match_labels
    //         .as_ref()
    //         .ok_or_else(|| {
    //             KubeApiError::missing_field(resource, ".selector or .selector.match_labels")
    //         })?
    //         .iter()
    //         .map(|(key, value)| format!("{key}={value}"))
    //         .collect::<Vec<String>>()
    //         .join(",");

    //     let list_params = ListParams {
    //         label_selector: Some(formatted_labels),
    //         ..Default::default()
    //     };

    //     let pod_api: Api<Pod> = get_k8s_resource_api(client,
    // resource.meta().namespace.as_deref());     let pods = pod_api.list(&list_params).await?;

    //     Ok(pods.items)
    // }
}
