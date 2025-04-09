use k8s_openapi::{
    api::{
        apps::v1::{Deployment, ReplicaSet, StatefulSet},
        core::v1::{PodTemplate, PodTemplateSpec},
    },
    apimachinery::pkg::apis::meta::v1::LabelSelector,
    Resource,
};
use kube::Client;
use serde::{Deserialize, Serialize};

use crate::{api::kubernetes::get_k8s_resource_api, error::KubeApiError};

/// A reference to some Kubernetes [workload](https://kubernetes.io/docs/concepts/workloads/) managed by an Argo [`Rollout`].
///
/// The documentation of Argo do not mention any restrictions no the referenced resource type -
/// "WorkloadRef holds a references to a workload that provides Pod template".
///
/// # Note
///
/// Information contained in this struct is not enough to fetch a dynamic resource (see
/// [`ApiResource`](kube::discovery::ApiResource)). What's more, there would be no way of knowing
/// how to extract the required [`PodTemplateSpec`] from the fetched resource.
///
/// Luckily, [source code](https://github.com/argoproj/argo-rollouts/blob/master/rollout/templateref.go#L41)
/// provides constraints on the resource type.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadRef {
    pub api_version: String,
    pub kind: String,
    pub name: String,
}

impl WorkloadRef {
    /// Fetched the referenced resource and extracts [`PodTemplateSpec`].
    /// Supports references to:
    /// 1. [`Deployment`]s
    /// 2. [`ReplicaSet`]s
    /// 3. [`PodTemplate`]s (uses `.metadata.labels`)
    /// 4. [`StatefulSet`]s
    pub async fn get_pod_template(
        &self,
        client: &Client,
        namespace: Option<&str>,
    ) -> Result<Option<PodTemplateSpec>, KubeApiError> {
        match (self.api_version.as_str(), self.kind.as_str()) {
            (Deployment::API_VERSION, Deployment::KIND) => {
                let mut deployment = get_k8s_resource_api::<Deployment>(client, namespace)
                    .get(&self.name)
                    .await?;

                deployment
                    .spec
                    .take()
                    .ok_or_else(|| KubeApiError::missing_field(&deployment, ".spec"))
                    .map(|spec| Some(spec.template))
            }
            (ReplicaSet::API_VERSION, ReplicaSet::KIND) => {
                let mut replica_set = get_k8s_resource_api::<ReplicaSet>(client, namespace)
                    .get(&self.name)
                    .await?;

                replica_set
                    .spec
                    .take()
                    .ok_or_else(|| KubeApiError::missing_field(&replica_set, ".spec"))?
                    .template
                    .ok_or_else(|| KubeApiError::missing_field(&replica_set, ".spec.template"))
                    .map(Some)
            }
            (PodTemplate::API_VERSION, PodTemplate::KIND) => {
                let mut pod_template = get_k8s_resource_api::<PodTemplate>(client, namespace)
                    .get(&self.name)
                    .await?;

                pod_template
                    .template
                    .take()
                    .ok_or_else(|| KubeApiError::missing_field(&pod_template, ".template"))
                    .map(Some)
            }
            (StatefulSet::API_VERSION, StatefulSet::KIND) => {
                let mut stateful_set = get_k8s_resource_api::<StatefulSet>(client, namespace)
                    .get(&self.name)
                    .await?;

                stateful_set
                    .spec
                    .take()
                    .ok_or_else(|| KubeApiError::missing_field(&stateful_set, ".spec"))
                    .map(|spec| Some(spec.template))
            }
            _ => Ok(None),
        }
    }

    /// Fetched the referenced resource and extracts [`LabelSelector`].
    /// Supports references to:
    /// 1. [`Deployment`]s
    /// 2. [`ReplicaSet`]s
    /// 3. [`PodTemplate`]s
    /// 4. [`StatefulSet`]s
    pub async fn get_match_labels(
        &self,
        client: &Client,
        namespace: Option<&str>,
    ) -> Result<Option<LabelSelector>, KubeApiError> {
        match (self.api_version.as_str(), self.kind.as_str()) {
            (Deployment::API_VERSION, Deployment::KIND) => {
                let mut deployment = get_k8s_resource_api::<Deployment>(client, namespace)
                    .get(&self.name)
                    .await?;

                deployment
                    .spec
                    .take()
                    .ok_or_else(|| KubeApiError::missing_field(&deployment, ".spec"))
                    .map(|spec| Some(spec.selector))
            }
            (ReplicaSet::API_VERSION, ReplicaSet::KIND) => {
                let mut replica_set = get_k8s_resource_api::<ReplicaSet>(client, namespace)
                    .get(&self.name)
                    .await?;

                replica_set
                    .spec
                    .take()
                    .ok_or_else(|| KubeApiError::missing_field(&replica_set, ".spec"))
                    .map(|spec| Some(spec.selector))
            }
            (PodTemplate::API_VERSION, PodTemplate::KIND) => {
                let mut pod_template = get_k8s_resource_api::<PodTemplate>(client, namespace)
                    .get(&self.name)
                    .await?;

                pod_template
                    .metadata
                    .labels
                    .take()
                    .ok_or_else(|| KubeApiError::missing_field(&pod_template, ".metadata.labels"))
                    .map(|match_labels| {
                        Some(LabelSelector {
                            match_labels: Some(match_labels),
                            ..Default::default()
                        })
                    })
            }
            (StatefulSet::API_VERSION, StatefulSet::KIND) => {
                let mut stateful_set = get_k8s_resource_api::<StatefulSet>(client, namespace)
                    .get(&self.name)
                    .await?;

                stateful_set
                    .spec
                    .take()
                    .ok_or_else(|| KubeApiError::missing_field(&stateful_set, ".spec"))
                    .map(|spec| Some(spec.selector))
            }
            _ => Ok(None),
        }
    }
}
