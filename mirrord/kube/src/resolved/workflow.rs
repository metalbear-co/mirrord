use kube::Client;

use super::ResolvedResource;
use crate::{
    api::{
        kubernetes::workflow::Workflow,
        runtime::{workflow::WorkflowRuntimeProvider, RuntimeData, RuntimeDataProvider},
    },
    error::Result,
};

impl RuntimeDataProvider for ResolvedResource<Workflow> {
    async fn runtime_data(&self, client: &Client, _namespace: Option<&str>) -> Result<RuntimeData> {
        WorkflowRuntimeProvider {
            client,
            resource: &self.resource,
            template: self.template.as_deref(),
            container: self.container.as_deref(),
        }
        .try_into_runtime_data()
        .await
    }
}
