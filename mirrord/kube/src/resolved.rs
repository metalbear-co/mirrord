use std::collections::BTreeMap;

use k8s_openapi::api::{
    apps::v1::{Deployment, StatefulSet},
    batch::v1::{CronJob, Job},
    core::v1::Pod,
};
use kube::{Client, Resource, ResourceExt};
use mirrord_config::{feature::network::incoming::ConcurrentSteal, target::Target};
use tracing::Level;

use super::{
    api::{
        kubernetes::{get_k8s_resource_api, rollout::Rollout},
        runtime::RuntimeData,
    },
    error::KubeApiError,
};

/// Helper struct for resolving user-provided [`Target`] to Kubernetes resources.
#[derive(Debug, Clone)]
pub enum ResolvedTarget {
    Deployment(Deployment, Option<String>),
    Rollout(Rollout, Option<String>),
    Job(Job, Option<String>),
    CronJob(CronJob, Option<String>),
    StatefulSet(StatefulSet, Option<String>),
    Pod(Pod, Option<String>),
    Targetless(String),
}

impl ResolvedTarget {
    /// Gets a target from k8s with the [`Client`] that is passed here, and checks if it's
    /// a valid mirrord target with [`ResolvedTarget::assert_valid_mirrord_target`].
    ///
    /// Currently this `client` comes set up with a mirrord-operator config.
    #[tracing::instrument(level = Level::DEBUG, skip(client), ret, err)]
    pub async fn new(
        client: &Client,
        target: &Target,
        namespace: Option<&str>,
    ) -> Result<Self, KubeApiError> {
        let target = match &target {
            Target::Deployment(target) => get_k8s_resource_api(client, namespace)
                .get(&target.deployment)
                .await
                .map(|deployment| ResolvedTarget::Deployment(deployment, target.container.clone())),
            Target::Rollout(target) => get_k8s_resource_api(client, namespace)
                .get(&target.rollout)
                .await
                .map(|rollout| ResolvedTarget::Rollout(rollout, target.container.clone())),
            Target::Job(target) => get_k8s_resource_api(client, namespace)
                .get(&target.job)
                .await
                .map(|job| ResolvedTarget::Job(job, target.container.clone())),
            Target::CronJob(target) => get_k8s_resource_api(client, namespace)
                .get(&target.cron_job)
                .await
                .map(|cron_job| ResolvedTarget::CronJob(cron_job, target.container.clone())),
            Target::StatefulSet(target) => get_k8s_resource_api(client, namespace)
                .get(&target.stateful_set)
                .await
                .map(|stateful_set| {
                    ResolvedTarget::StatefulSet(stateful_set, target.container.clone())
                }),
            Target::Pod(pod_target) => get_k8s_resource_api(client, namespace)
                .get(&pod_target.pod)
                .await
                .map(|pod| ResolvedTarget::Pod(pod, pod_target.container.clone())),
            Target::Targetless => Ok(ResolvedTarget::Targetless(
                namespace.unwrap_or("default").to_string(),
            )),
        }?;

        Ok(target)
    }

    /// Check if the target can be used as a mirrord target.
    ///
    /// 1. [`ResolvedTarget::Deployment`] or [`ResolvedTarget::Rollout`] - has available replicas
    ///    and the target container, if specified, is found in the spec
    /// 2. [`ResolvedTarget::Pod`] - passes target-readiness check of `StatusObserver`
    /// 3. [`ResolvedTarget::Job`] - error, as this is `copy_target` exclusive
    /// 4. [`ResolvedTarget::Targetless`] - no check
    #[tracing::instrument(level = Level::DEBUG, skip(client), ret, err)]
    pub async fn assert_valid_mirrord_target(self, client: &Client) -> Result<Self, KubeApiError> {
        match &self {
            ResolvedTarget::Deployment(deployment, container) => {
                let available = deployment
                    .status
                    .as_ref()
                    .ok_or_else(|| KubeApiError::missing_field(deployment, ".status"))?
                    .available_replicas
                    .unwrap_or_default(); // Field can be missing when there are no replicas

                if available <= 0 {
                    return Err(KubeApiError::invalid_state(
                        deployment,
                        "no available replicas",
                    ));
                }

                if let Some(container) = container {
                    // verify that the container exists
                    deployment
                        .spec
                        .as_ref()
                        .ok_or_else(|| KubeApiError::missing_field(deployment, ".spec"))?
                        .template
                        .spec
                        .as_ref()
                        .ok_or_else(|| KubeApiError::missing_field(deployment, ".spec.template.spec"))?
                        .containers
                        .iter()
                        .find(|c| c.name == *container)
                        .ok_or_else(|| KubeApiError::invalid_state(deployment, format_args!("specified pod template does not contain target container `{container}`")))?;
                }
            }
            ResolvedTarget::Pod(pod, container) => {
                get_full_runtime_data(pod, container.as_deref())?;
            }

            ResolvedTarget::Rollout(rollout, container) => {
                let available = rollout
                    .status
                    .as_ref()
                    .ok_or_else(|| KubeApiError::missing_field(rollout, ".status"))?
                    .available_replicas
                    .unwrap_or_default(); // Field can be missing when there are no replicas

                if available <= 0 {
                    return Err(KubeApiError::invalid_state(
                        rollout,
                        "no available replicas",
                    ));
                }

                let pod_template = rollout.get_pod_template(client).await?;

                if let Some(container) = container {
                    // verify that the container exists
                    pod_template.spec.as_ref().ok_or_else(|| KubeApiError::invalid_state(rollout, "specified pod template is missing field `.spec`"))?
                    .containers
                                .iter()
                                .find(|c| c.name == *container)
                                .ok_or_else(|| KubeApiError::invalid_state(rollout, format_args!("specified pod template does not contain target container `{container}`")))?;
                }
            }

            ResolvedTarget::Job(..) => {
                return Err(KubeApiError::requires_copy::<Job>());
            }
            ResolvedTarget::CronJob(..) => {
                return Err(KubeApiError::requires_copy::<CronJob>());
            }
            ResolvedTarget::StatefulSet(stateful_set, container) => {
                let available = stateful_set
                    .status
                    .as_ref()
                    .ok_or_else(|| KubeApiError::missing_field(stateful_set, ".status"))?
                    .available_replicas
                    .unwrap_or_default(); // Field can be missing when there are no replicas

                if available <= 0 {
                    return Err(KubeApiError::invalid_state(
                        stateful_set,
                        "no available replicas",
                    ));
                }

                if let Some(container) = container {
                    // verify that the container exists
                    stateful_set
                        .spec
                        .as_ref()
                        .ok_or_else(|| KubeApiError::missing_field(stateful_set, ".spec"))?
                        .template
                        .spec
                        .as_ref()
                        .ok_or_else(|| KubeApiError::missing_field(stateful_set, ".spec.template.spec"))?
                        .containers
                        .iter()
                        .find(|c| c.name == *container)
                        .ok_or_else(|| KubeApiError::invalid_state(stateful_set, format_args!("specified pod template does not contain target container `{container}`")))?;
                }
            }

            ResolvedTarget::Targetless(_) => {
                // no check needed here
            }
        }

        Ok(self)
    }

    pub fn connect_url(
        &self,
        use_proxy: bool,
        concurrent_steal: ConcurrentSteal,
        api_version: &str,
        plural: &str,
        url_path: &str,
    ) -> Result<String, KubeApiError> {
        let name = self.urlfied_name();
        let namespace = self.namespace().unwrap_or("default");

        let url = if use_proxy {
            format!("/apis/{api_version}/proxy/namespaces/{namespace}/{plural}/{name}?on_concurrent_steal={concurrent_steal}&connect=true")
        } else {
            format!("{url_path}/{name}?on_concurrent_steal={concurrent_steal}&connect=true")
        };

        Ok(url)
    }

    pub fn urlfied_name(&self) -> String {
        // TODO(alex) [high] 1: `type_()` here will return `deployment`, but mirrord policies
        // support both `deploy` and `deployment` ...
        let mut url = self.type_().to_string();

        if let Some(target_name) = self.name() {
            url.push_str(&format!(".{target_name}"));
        }

        if let Some(container) = self.container() {
            url.push_str(&format!(".container.{container}"));
        }

        url
    }

    /// Convenient way of getting the container from this target.
    pub fn container(&self) -> Option<&str> {
        match self {
            ResolvedTarget::Deployment(_, container)
            | ResolvedTarget::Rollout(_, container)
            | ResolvedTarget::Job(_, container)
            | ResolvedTarget::CronJob(_, container)
            | ResolvedTarget::StatefulSet(_, container)
            | ResolvedTarget::Pod(_, container) => container.as_deref(),
            ResolvedTarget::Targetless(..) => None,
        }
    }

    /// Is this a [`ResolvedTarget::Deployment`], and is it empty?
    pub fn empty_deployment(&self) -> bool {
        if let Self::Deployment(target, _) = self {
            !target
                .status
                .as_ref()
                .map(|status| status.available_replicas > Some(0))
                .unwrap_or_default()
        } else {
            false
        }
    }

    /// Returns the number of containers for this [`ResolvedTarget`], defaulting to 1.
    pub fn containers_count(&self) -> usize {
        match self {
            ResolvedTarget::Deployment(target, _) => target
                .spec
                .as_ref()
                .and_then(|spec| spec.template.spec.as_ref())
                .map(|pod_spec| pod_spec.containers.len()),
            ResolvedTarget::Rollout(target, _) => target
                .spec
                .as_ref()
                .and_then(|spec| spec.template.as_ref())
                .and_then(|pod_template| pod_template.spec.as_ref())
                .map(|pod_spec| pod_spec.containers.len()),
            ResolvedTarget::StatefulSet(target, _) => target
                .spec
                .as_ref()
                .and_then(|spec| spec.template.spec.as_ref())
                .map(|pod_spec| pod_spec.containers.len()),
            ResolvedTarget::CronJob(target, _) => target
                .spec
                .as_ref()
                .and_then(|spec| spec.job_template.spec.as_ref())
                .and_then(|job_spec| job_spec.template.spec.as_ref())
                .map(|pod_spec| pod_spec.containers.len()),
            ResolvedTarget::Job(target, _) => target
                .spec
                .as_ref()
                .and_then(|spec| spec.template.spec.as_ref())
                .map(|pod_spec| pod_spec.containers.len()),
            ResolvedTarget::Pod(target, _) => target
                .spec
                .as_ref()
                .map(|pod_spec| pod_spec.containers.len()),
            ResolvedTarget::Targetless(..) => Some(1),
        }
        .unwrap_or(1)
    }

    /// Convert [`ResolvedTarget`] into `<Target>.metadata.labels`
    pub fn into_labels(self) -> Option<BTreeMap<String, String>> {
        match self {
            ResolvedTarget::Deployment(deployment, _) => deployment.metadata.labels,
            ResolvedTarget::Rollout(rollout, _) => rollout.metadata.labels,
            ResolvedTarget::Pod(pod, _) => pod.metadata.labels,
            ResolvedTarget::Job(job, _) => job.metadata.labels,
            ResolvedTarget::CronJob(cron_job, _) => cron_job.metadata.labels,
            ResolvedTarget::StatefulSet(stateful_set, _) => stateful_set.metadata.labels,
            ResolvedTarget::Targetless(_) => None,
        }
    }

    pub fn type_(&self) -> &str {
        match self {
            ResolvedTarget::Deployment(_, _) => "deployment",
            ResolvedTarget::Rollout(_, _) => "rollout",
            ResolvedTarget::Pod(_, _) => "pod",
            ResolvedTarget::Job(_, _) => "job",
            ResolvedTarget::CronJob(_, _) => "cronjob",
            ResolvedTarget::StatefulSet(_, _) => "statefulset",
            ResolvedTarget::Targetless(_) => "targetless",
        }
    }

    pub fn get_container(&self) -> Option<&str> {
        match self {
            ResolvedTarget::Deployment(_, container)
            | ResolvedTarget::Rollout(_, container)
            | ResolvedTarget::Job(_, container)
            | ResolvedTarget::CronJob(_, container)
            | ResolvedTarget::StatefulSet(_, container)
            | ResolvedTarget::Pod(_, container) => container.as_deref(),
            ResolvedTarget::Targetless(..) => None,
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            ResolvedTarget::Deployment(deployment, _) => deployment.metadata.name.as_deref(),
            ResolvedTarget::Rollout(rollout, _) => rollout.meta().name.as_deref(),
            ResolvedTarget::Pod(pod, _) => pod.metadata.name.as_deref(),
            ResolvedTarget::Job(job, _) => job.metadata.name.as_deref(),
            ResolvedTarget::CronJob(cron_job, _) => cron_job.metadata.name.as_deref(),
            ResolvedTarget::StatefulSet(stateful_set, _) => stateful_set.metadata.name.as_deref(),
            ResolvedTarget::Targetless(_) => None,
        }
    }

    pub fn name_any(&self) -> String {
        match self {
            ResolvedTarget::Deployment(dep, _) => dep.name_any(),
            ResolvedTarget::Rollout(roll, _) => roll.name_any(),
            ResolvedTarget::Pod(pod, _) => pod.name_any(),
            ResolvedTarget::Job(job, _) => job.name_any(),
            ResolvedTarget::CronJob(cj, _) => cj.name_any(),
            ResolvedTarget::StatefulSet(set, _) => set.name_any(),
            ResolvedTarget::Targetless(..) => "targetless".to_string(),
        }
    }

    pub fn namespace(&self) -> Option<&str> {
        match self {
            ResolvedTarget::Deployment(deployment, _) => deployment.metadata.namespace.as_deref(),
            ResolvedTarget::Rollout(rollout, _) => rollout.meta().namespace.as_deref(),
            ResolvedTarget::Pod(pod, _) => pod.metadata.namespace.as_deref(),
            ResolvedTarget::Job(job, _) => job.metadata.namespace.as_deref(),
            ResolvedTarget::CronJob(cron_job, _) => cron_job.metadata.namespace.as_deref(),
            ResolvedTarget::StatefulSet(stateful_set, _) => {
                stateful_set.metadata.namespace.as_deref()
            }
            ResolvedTarget::Targetless(namespace) => Some(namespace),
        }
    }
}

/// Checks target-readiness of a [`Pod`]. The given [`Pod`] is considered target-ready when
/// [`RuntimeData::from_pod`] succeeds.
///
/// # Return
///
/// Returns [`RuntimeData`] extracted from the [`Pod`] and restart count of the selected
/// container.
pub fn get_full_runtime_data(
    pod: &Pod,
    container_name: Option<&str>,
) -> Result<(RuntimeData, i32), KubeApiError> {
    let runtime_data = RuntimeData::from_pod(pod, container_name)?;

    let restart_count = pod
        .status
        .as_ref()
        .and_then(|status| status.container_statuses.as_ref())
        .into_iter()
        .flatten()
        .find(|status| status.name == runtime_data.container_name)
        .expect(
            "container should exist in containerStatuses, we just found it with RuntimeData::from_pod",
        )
        .restart_count;

    Ok((runtime_data, restart_count))
}
