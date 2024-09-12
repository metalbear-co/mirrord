use k8s_openapi::api::{
    apps::v1::{Deployment, StatefulSet},
    batch::v1::{CronJob, Job},
    core::v1::Pod,
};
use kube::Client;
use mirrord_config::target::Target;
use mirrord_kube::{
    api::{
        kubernetes::{get_k8s_resource_api, rollout::Rollout},
        runtime::RuntimeData,
    },
    error::KubeApiError,
};

/// Helper struct for resolving user-provided [`Target`] to Kubernetes resources.
/// visibility/permissions requirements of the operator.
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
        }?
        .assert_valid_mirrord_target(client)
        .await?;

        Ok(target)
    }

    /// Check if the target can be used as a mirrord target.
    ///
    /// 1. [`ResolvedTarget::Deployment`] or [`ResolvedTarget::Rollout`] - has available replicas
    ///    and the target container, if specified, is found in the spec
    /// 2. [`ResolvedTarget::Pod`] - passes target-readiness check of
    ///    [`StatusObserver`](operator_context::target::status::StatusObserver)
    /// 3. [`ResolvedTarget::Job`] - error, as this is `copy_target` exclusive
    /// 4. [`ResolvedTarget::Targetless`] - no check
    async fn assert_valid_mirrord_target(self, client: &Client) -> Result<Self, KubeApiError> {
        // TODO(alex) [high] 11: Validate non-operator mirrord requirements? Could probably
        // take the `with_operator` bool here as well?

        match &self {
            ResolvedTarget::Deployment(deployment, container) => {
                let available = deployment
                    .status
                    .as_ref()
                    .ok_or_else(|| KubeApiError::missing_field(deployment, ".status"))?
                    .available_replicas
                    .unwrap_or_default(); // Field can be missing when there are no replicas

                if available <= 0 {
                    return Err(
                        KubeApiError::invalid_state(deployment, "no available replicas").into(),
                    );
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
                    return Err(
                        KubeApiError::invalid_state(rollout, "no available replicas").into(),
                    );
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
                return Err(KubeApiError::requires_copy::<Job>().into());
            }
            ResolvedTarget::CronJob(..) => {
                return Err(KubeApiError::requires_copy::<CronJob>().into());
            }
            ResolvedTarget::StatefulSet(stateful_set, container) => {
                let available = stateful_set
                    .status
                    .as_ref()
                    .ok_or_else(|| KubeApiError::missing_field(stateful_set, ".status"))?
                    .available_replicas
                    .unwrap_or_default(); // Field can be missing when there are no replicas

                if available <= 0 {
                    return Err(
                        KubeApiError::invalid_state(stateful_set, "no available replicas").into(),
                    );
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

    pub(super) fn container(&self) -> Option<&str> {
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

    pub(super) fn empty_deployment(&self) -> bool {
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

    pub(super) fn containers_count(&self) -> usize {
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
