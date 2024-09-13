use std::borrow::Cow;

use k8s_openapi::{
    api::{
        apps::v1::{Deployment, ReplicaSet, StatefulSet},
        authorization::v1::{ResourceAttributes, SubjectAccessReview, SubjectAccessReviewStatus},
        core::v1::{PodTemplate, PodTemplateSpec},
    },
    apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta},
    ListableResource, Metadata, NamespaceResourceScope, Resource,
};
use kube::{api::PostParams, Api, Client};
use serde::{de, Deserialize, Serialize};

use super::get_k8s_resource_api;
use crate::{error::KubeApiError, subject_access_review::SubjectAccessReviewSource};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rollout {
    metadata: ObjectMeta,
    pub spec: Option<RolloutSpec>,
    pub status: Option<RolloutStatus>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RolloutStatus {
    pub available_replicas: Option<i32>,
    /// Looks like this is a string for some reason:
    /// [rollouts/v1alpha1/types.go](https://github.com/argoproj/argo-rollouts/blob/4f1edbe9332b93d8aaf1d8f34239da6f952b8a93/pkg/apis/rollouts/v1alpha1/types.go#L922)
    pub observed_generation: Option<String>,
}

/// Argo [`Rollout`]s provide `Pod` template in one of two ways:
/// 1. Inline (`template` field).
/// 2. Via a reference to some Kubernetes workload (`workloadRef` field).
///
/// See [Rollout spec](https://argoproj.github.io/argo-rollouts/features/specification/) for reference.
#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RolloutSpec {
    pub selector: LabelSelector,
    #[serde(deserialize_with = "rollout_pod_spec")]
    pub template: Option<PodTemplateSpec>,
    pub workload_ref: Option<WorkloadRef>,
}

/// A reference to some Kubernetes [workload](https://kubernetes.io/docs/concepts/workloads/) managed by an Argo [`Rollout`].
/// The documentation of Argo do not mention any restrictions no the referenced resource type -
/// "WorkloadRef holds a references to a workload that provides Pod template".
///
/// # Note
///
/// Information contained in this struct is not enough to fetch a dynamic resource (see
/// [`ApiResource`](kube::discovery::ApiResource)). What's more, there would be no way of knowing
/// how to extract the required [`PodTemplateSpec`] from the fetched resource.
///
/// Luckily, [source code](https://github.com/argoproj/argo-rollouts/blob/master/rollout/templateref.go#L41) provides constraints on the resource type.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadRef {
    api_version: String,
    kind: String,
    name: String,
}

/// Custom deserializer for a rollout template field due to
/// [#548](https://github.com/metalbear-co/operator/issues/548)
/// First deserializes it as value, fixes possible issues and then deserializes it as
/// PodTemplateSpec.
fn rollout_pod_spec<'de, D>(deserializer: D) -> Result<Option<PodTemplateSpec>, D::Error>
where
    D: de::Deserializer<'de>,
{
    let mut value = serde_json::Value::deserialize(deserializer)?;

    value
        .get_mut("spec")
        .and_then(|spec| spec.get_mut("containers")?.as_array_mut())
        .into_iter()
        .flatten()
        .filter_map(|container| container.get_mut("resources"))
        .for_each(|resources| {
            for field in ["limits", "requests"] {
                let Some(object) = resources.get_mut(field) else {
                    continue;
                };

                for field in ["cpu", "memory"] {
                    let Some(raw) = object.get_mut(field) else {
                        continue;
                    };

                    if let Some(number) = raw.as_number() {
                        *raw = number.to_string().into();
                    }
                }
            }
        });

    let pod_template: PodTemplateSpec = serde_json::from_value(value).map_err(de::Error::custom)?;
    Ok(Some(pod_template))
}

impl Rollout {
    /// Get the pod template spec out of a rollout spec.
    /// Make requests to k8s if necessary when the template is not directly included in the
    /// rollout spec ([`RolloutSpec::template`]), but only referenced via a workload_ref
    /// ([`RolloutSpec::workload_ref`]).
    pub async fn get_pod_template<'a>(
        &'a self,
        client: &Client,
    ) -> Result<Cow<'a, PodTemplateSpec>, KubeApiError> {
        let spec = self
            .spec
            .as_ref()
            .ok_or_else(|| KubeApiError::missing_field(self, ".spec"))?;

        match spec {
            RolloutSpec {
                template: Some(..),
                workload_ref: Some(..),
                ..
            } => Err(KubeApiError::invalid_state(
                self,
                "both `.spec.template` and `.spec.workladRef` fields are filled",
            )),

            RolloutSpec {
                template: None,
                workload_ref: None,
                ..
            } => Err(KubeApiError::invalid_state(
                self,
                "both `.spec.template` and `.spec.workloadRef` fields are empty",
            )),

            RolloutSpec {
                template: Some(template),
                ..
            } => Ok(Cow::Borrowed(template)),

            RolloutSpec {
                workload_ref: Some(workload_ref),
                ..
            } => workload_ref
                .get_pod_template(client, self.metadata.namespace.as_deref())
                .await?
                .ok_or_else(|| {
                    KubeApiError::invalid_state(
                        self,
                        format_args!(
                            "field `.spec.workloadRef` refers to an unknown resource `{}/{}`",
                            workload_ref.api_version, workload_ref.kind
                        ),
                    )
                })
                .map(Cow::Owned),
        }
    }

    /// Try and perform a nested [`SubjectAccessReview`] request if using `workload_ref` otherwise
    /// will result in `Ok(None)` where this should be considered as equivalent to `allowed == true`
    pub async fn get_pod_template_access<'a, S>(
        &'a self,
        client: &Client,
        subject_access_source: &S,
    ) -> Result<Option<SubjectAccessReviewStatus>, KubeApiError>
    where
        S: SubjectAccessReviewSource,
    {
        let spec = self
            .spec
            .as_ref()
            .ok_or_else(|| KubeApiError::missing_field(self, ".spec"))?;

        match spec {
            RolloutSpec {
                workload_ref: Some(workload_ref),
                ..
            } => workload_ref
                .get_pod_template_access(
                    client,
                    self.metadata.namespace.as_deref(),
                    subject_access_source,
                )
                .await
                .map(Some),
            _ => Ok(None),
        }
    }
}

impl WorkloadRef {
    /// Fetched the referenced resource and extracts [`PodTemplateSpec`].
    /// Supports references to:
    /// 1. [`Deployment`]s
    /// 2. [`ReplicaSet`]s
    /// 3. [`PodTemplate`]s
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

    /// Check weather the user has access to resource referanced by this [`WorkloadRef`].
    pub async fn get_pod_template_access<S>(
        &self,
        client: &Client,
        namespace: Option<&str>,
        subject_access_source: &S,
    ) -> Result<SubjectAccessReviewStatus, KubeApiError>
    where
        S: SubjectAccessReviewSource,
    {
        let subject_access_review_api = Api::<SubjectAccessReview>::all(client.clone());

        // Split "apps/v1" to ("apps", "v1") and core apis like "v1" to ("", "v1")
        let (group, version) = self
            .api_version
            .split_once('/')
            .unwrap_or(("", self.api_version.as_str()));

        let subject_review =
            subject_access_source.create_subject_access_review(ResourceAttributes {
                group: Some(group.to_owned()),
                name: Some(self.name.clone()),
                namespace: namespace.map(str::to_owned),
                resource: Some(format!("{}s", self.kind.to_lowercase())),
                verb: Some("get".into()),
                version: Some(version.to_owned()),
                ..Default::default()
            });

        let mut access = subject_access_review_api
            .create(&PostParams::default(), &subject_review)
            .await?;

        access
            .status
            .take()
            .ok_or_else(|| KubeApiError::missing_field(&access, ".status"))
    }
}

impl Resource for Rollout {
    const API_VERSION: &'static str = "argoproj.io/v1alpha1";
    const GROUP: &'static str = "argoproj.io";
    const KIND: &'static str = "Rollout";
    const VERSION: &'static str = "v1alpha1";
    const URL_PATH_SEGMENT: &'static str = "rollouts";
    type Scope = NamespaceResourceScope;
}

impl ListableResource for Rollout {
    const LIST_KIND: &'static str = "RolloutList";
}

impl Metadata for Rollout {
    type Ty = ObjectMeta;

    fn metadata(&self) -> &Self::Ty {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut Self::Ty {
        &mut self.metadata
    }
}
