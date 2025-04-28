use kube::Client;

use super::{ResolvedResource, WorkflowTargetLookup};
use crate::{
    api::{
        kubernetes::workflow::Workflow,
        runtime::{workflow::WorkflowRuntimeProvider, RuntimeData, RuntimeDataProvider},
    },
    error::Result,
};

impl RuntimeDataProvider for (&ResolvedResource<Workflow>, &WorkflowTargetLookup<true>) {
    async fn runtime_data(&self, client: &Client, _namespace: Option<&str>) -> Result<RuntimeData> {
        let (resolved, lookup) = self;

        WorkflowRuntimeProvider {
            client,
            resource: &resolved.resource,
            container: resolved.container.as_deref(),
            template: lookup.template.as_deref(),
            step: lookup.step.as_deref(),
        }
        .try_into_runtime_data()
        .await
    }
}
