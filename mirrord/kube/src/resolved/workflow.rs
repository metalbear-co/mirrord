use kube::Client;

use super::{ResolvedResource, WorkflowTargetLookup};
use crate::{
    api::{
        kubernetes::workflow::Workflow,
        runtime::{workflow::WorkflowRuntimeProvider, RuntimeData, RuntimeDataProvider},
    },
    error::Result,
};

impl RuntimeDataProvider for (&ResolvedResource<Workflow>, &WorkflowTargetLookup) {
    async fn runtime_data(&self, client: &Client, _namespace: Option<&str>) -> Result<RuntimeData> {
        let (resolved, lookup) = self;

        WorkflowRuntimeProvider {
            client,
            resource: &resolved.resource,
            template: lookup.template(),
            step: lookup.step(),
            container: resolved.container.as_deref(),
        }
        .try_into_runtime_data()
        .await
    }
}
