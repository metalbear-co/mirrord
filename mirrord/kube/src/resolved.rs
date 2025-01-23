use std::collections::BTreeMap;

use k8s_openapi::api::{
    apps::v1::{Deployment, StatefulSet},
    batch::v1::{CronJob, Job},
    core::v1::{Pod, Service},
};
use kube::{Client, Resource, ResourceExt};
use mirrord_config::{feature::network::incoming::ConcurrentSteal, target::Target};
use tracing::Level;

use super::{
    api::{kubernetes::get_k8s_resource_api, runtime::RuntimeData},
    error::KubeApiError,
};
use crate::api::{kubernetes::rollout::Rollout, runtime::RuntimeDataFromLabels};

pub mod cron_job;
pub mod deployment;
pub mod job;
pub mod pod;
pub mod rollout;
pub mod service;
pub mod stateful_set;

/// Helper struct for resolving user-provided [`Target`] to Kubernetes resources.
///
/// It has 3 implementations based on `CHECKED`, which indicates if this target has been
/// checked with [`ResolvedTarget::assert_valid_mirrord_target`].
///
/// 1. A generic implementation with helper methods for getting strings such as names, types and so
///    on;
/// 2. `CHECKED = false` that may be used to build the struct, and to call
///    [`ResolvedTarget::assert_valid_mirrord_target`] (along with the generic methods);
/// 3. `CHECKED = true` which is how we get a connection url for the target;
#[derive(Debug, Clone)]
pub enum ResolvedTarget<const CHECKED: bool> {
    Deployment(ResolvedResource<Deployment>),
    Rollout(ResolvedResource<Rollout>),
    Job(ResolvedResource<Job>),
    CronJob(ResolvedResource<CronJob>),
    StatefulSet(ResolvedResource<StatefulSet>),
    Service(ResolvedResource<Service>),

    /// [`Pod`] is a special case, in that it does not implement [`RuntimeDataFromLabels`],
    /// and instead we implement a `runtime_data` method directly in its
    /// [`ResolvedResource<Pod>`] impl.
    Pod(ResolvedResource<Pod>),

    Targetless(
        /// Agent pod's namespace.
        String,
    ),
}

/// A kubernetes [`Resource`], and container pair to be used based on the target we
/// resolved, see [`ResolvedTarget`].
///
/// Having this be its own type (instead of just a tuple) lets us implement the
/// [`RuntimeDataFromLabels`] trait, which is part of finding out the pod we want to
/// target via the [`RuntimeDataProvider`](crate::api::runtime::RuntimeDataProvider).
#[derive(Debug, Clone)]
pub struct ResolvedResource<R>
where
    R: Resource,
{
    pub resource: R,
    pub container: Option<String>,
}

impl<const CHECKED: bool> ResolvedTarget<CHECKED> {
    pub fn name(&self) -> Option<&str> {
        match self {
            ResolvedTarget::Deployment(ResolvedResource { resource, .. }) => {
                resource.metadata.name.as_deref()
            }
            ResolvedTarget::Rollout(ResolvedResource { resource, .. }) => {
                resource.meta().name.as_deref()
            }
            ResolvedTarget::Pod(ResolvedResource { resource, .. }) => {
                resource.metadata.name.as_deref()
            }
            ResolvedTarget::Job(ResolvedResource { resource, .. }) => {
                resource.metadata.name.as_deref()
            }
            ResolvedTarget::CronJob(ResolvedResource { resource, .. }) => {
                resource.metadata.name.as_deref()
            }
            ResolvedTarget::StatefulSet(ResolvedResource { resource, .. }) => {
                resource.metadata.name.as_deref()
            }
            ResolvedTarget::Service(ResolvedResource { resource, .. }) => {
                resource.metadata.name.as_deref()
            }
            ResolvedTarget::Targetless(_) => None,
        }
    }

    pub fn name_any(&self) -> String {
        match self {
            ResolvedTarget::Deployment(ResolvedResource { resource, .. }) => resource.name_any(),
            ResolvedTarget::Rollout(ResolvedResource { resource, .. }) => resource.name_any(),
            ResolvedTarget::Pod(ResolvedResource { resource, .. }) => resource.name_any(),
            ResolvedTarget::Job(ResolvedResource { resource, .. }) => resource.name_any(),
            ResolvedTarget::CronJob(ResolvedResource { resource, .. }) => resource.name_any(),
            ResolvedTarget::StatefulSet(ResolvedResource { resource, .. }) => resource.name_any(),
            ResolvedTarget::Service(ResolvedResource { resource, .. }) => resource.name_any(),
            ResolvedTarget::Targetless(..) => "targetless".to_string(),
        }
    }

    pub fn namespace(&self) -> Option<&str> {
        match self {
            ResolvedTarget::Deployment(ResolvedResource { resource, .. }) => {
                resource.metadata.namespace.as_deref()
            }
            ResolvedTarget::Rollout(ResolvedResource { resource, .. }) => {
                resource.meta().namespace.as_deref()
            }
            ResolvedTarget::Pod(ResolvedResource { resource, .. }) => {
                resource.metadata.namespace.as_deref()
            }
            ResolvedTarget::Job(ResolvedResource { resource, .. }) => {
                resource.metadata.namespace.as_deref()
            }
            ResolvedTarget::CronJob(ResolvedResource { resource, .. }) => {
                resource.metadata.namespace.as_deref()
            }
            ResolvedTarget::StatefulSet(ResolvedResource { resource, .. }) => {
                resource.metadata.namespace.as_deref()
            }
            ResolvedTarget::Service(ResolvedResource { resource, .. }) => {
                resource.metadata.namespace.as_deref()
            }
            ResolvedTarget::Targetless(namespace) => Some(namespace),
        }
    }

    /// Convert [`ResolvedTarget`] into `<Target>.metadata.labels`
    pub fn into_labels(self) -> Option<BTreeMap<String, String>> {
        match self {
            ResolvedTarget::Deployment(ResolvedResource { resource, .. }) => {
                resource.metadata.labels
            }
            ResolvedTarget::Rollout(ResolvedResource { resource, .. }) => resource.metadata.labels,
            ResolvedTarget::Pod(ResolvedResource { resource, .. }) => resource.metadata.labels,
            ResolvedTarget::Job(ResolvedResource { resource, .. }) => resource.metadata.labels,
            ResolvedTarget::CronJob(ResolvedResource { resource, .. }) => resource.metadata.labels,
            ResolvedTarget::StatefulSet(ResolvedResource { resource, .. }) => {
                resource.metadata.labels
            }
            ResolvedTarget::Service(ResolvedResource { resource, .. }) => resource.metadata.labels,
            ResolvedTarget::Targetless(_) => None,
        }
    }

    pub fn type_(&self) -> &str {
        match self {
            ResolvedTarget::Deployment(_) => "deployment",
            ResolvedTarget::Rollout(_) => "rollout",
            ResolvedTarget::Pod(_) => "pod",
            ResolvedTarget::Job(_) => "job",
            ResolvedTarget::CronJob(_) => "cronjob",
            ResolvedTarget::StatefulSet(_) => "statefulset",
            ResolvedTarget::Service(_) => "service",
            ResolvedTarget::Targetless(_) => "targetless",
        }
    }

    /// Convenient way of getting the container from this target.
    pub fn container(&self) -> Option<&str> {
        match self {
            ResolvedTarget::Deployment(ResolvedResource { container, .. })
            | ResolvedTarget::Rollout(ResolvedResource { container, .. })
            | ResolvedTarget::Job(ResolvedResource { container, .. })
            | ResolvedTarget::CronJob(ResolvedResource { container, .. })
            | ResolvedTarget::StatefulSet(ResolvedResource { container, .. })
            | ResolvedTarget::Service(ResolvedResource { container, .. })
            | ResolvedTarget::Pod(ResolvedResource { container, .. }) => container.as_deref(),
            ResolvedTarget::Targetless(..) => None,
        }
    }

    /// Is this a [`ResolvedTarget::Deployment`], and is it empty?
    pub fn empty_deployment(&self) -> bool {
        if let Self::Deployment(ResolvedResource { resource, .. }) = self {
            !resource
                .status
                .as_ref()
                .map(|status| status.available_replicas > Some(0))
                .unwrap_or_default()
        } else {
            false
        }
    }
}

impl ResolvedTarget<false> {
    /// Gets a target from k8s with the [`Client`] that is passed here.
    /// Currently this `client` comes set up with a mirrord-operator config.
    #[tracing::instrument(level = Level::DEBUG, skip(client), ret, err)]
    pub async fn new(
        client: &Client,
        target: &Target,
        namespace: Option<&str>,
    ) -> Result<Self, KubeApiError> {
        let target = match &target {
            Target::Deployment(target) => get_k8s_resource_api::<Deployment>(client, namespace)
                .get(&target.deployment)
                .await
                .map(|resource| {
                    ResolvedTarget::Deployment(ResolvedResource {
                        resource,
                        container: target.container.clone(),
                    })
                }),
            Target::Rollout(target) => get_k8s_resource_api::<Rollout>(client, namespace)
                .get(&target.rollout)
                .await
                .map(|resource| {
                    ResolvedTarget::Rollout(ResolvedResource {
                        resource,
                        container: target.container.clone(),
                    })
                }),
            Target::Job(target) => get_k8s_resource_api::<Job>(client, namespace)
                .get(&target.job)
                .await
                .map(|resource| {
                    ResolvedTarget::Job(ResolvedResource {
                        resource,
                        container: target.container.clone(),
                    })
                }),
            Target::CronJob(target) => get_k8s_resource_api::<CronJob>(client, namespace)
                .get(&target.cron_job)
                .await
                .map(|resource| {
                    ResolvedTarget::CronJob(ResolvedResource {
                        resource,
                        container: target.container.clone(),
                    })
                }),
            Target::StatefulSet(target) => get_k8s_resource_api::<StatefulSet>(client, namespace)
                .get(&target.stateful_set)
                .await
                .map(|resource| {
                    ResolvedTarget::StatefulSet(ResolvedResource {
                        resource,
                        container: target.container.clone(),
                    })
                }),
            Target::Pod(target) => get_k8s_resource_api::<Pod>(client, namespace)
                .get(&target.pod)
                .await
                .map(|resource| {
                    ResolvedTarget::Pod(ResolvedResource {
                        resource,
                        container: target.container.clone(),
                    })
                }),
            Target::Service(target) => get_k8s_resource_api::<Service>(client, namespace)
                .get(&target.service)
                .await
                .map(|resource| {
                    ResolvedTarget::Service(ResolvedResource {
                        resource,
                        container: target.container.clone(),
                    })
                }),
            Target::Targetless => Ok(ResolvedTarget::Targetless(
                namespace.unwrap_or("default").to_string(),
            )),
        }?;

        Ok(target)
    }

    /// Checks if the target can be used via the mirrord Operator.
    ///
    /// This is implemented in the CLI only to improve the UX (skip roundtrip to the operator).
    ///
    /// Performs only basic checks:
    /// 1. [`ResolvedTarget::Deployment`], [`ResolvedTarget::Rollout`],
    ///    [`ResolvedTarget::StatefulSet`] - has available replicas and the target container, if
    ///    specified, is found in the spec
    /// 2. [`ResolvedTarget::Pod`] - passes target-readiness check, see [`RuntimeData::from_pod`].
    /// 3. [`ResolvedTarget::Job`] and [`ResolvedTarget::CronJob`] - error, as this is `copy_target`
    ///    exclusive
    /// 4. [`ResolvedTarget::Targetless`] - no check (not applicable)
    /// 5. [`ResolvedTarget::Service`] - has available replicas and the target container, if
    ///    specified, is found in at least one of them
    #[tracing::instrument(level = Level::DEBUG, skip(client), ret, err)]
    pub async fn assert_valid_mirrord_target(
        self,
        client: &Client,
    ) -> Result<ResolvedTarget<true>, KubeApiError> {
        match self {
            ResolvedTarget::Deployment(ResolvedResource {
                resource,
                container,
            }) => {
                let available = resource
                    .status
                    .as_ref()
                    .ok_or_else(|| KubeApiError::missing_field(&resource, ".status"))?
                    .available_replicas
                    .unwrap_or_default(); // Field can be missing when there are no replicas

                if available <= 0 {
                    return Err(KubeApiError::invalid_state(
                        &resource,
                        "no available replicas",
                    ));
                }

                if let Some(container) = &container {
                    // verify that the container exists
                    resource
                        .spec
                        .as_ref()
                        .ok_or_else(|| KubeApiError::missing_field(&resource, ".spec"))?
                        .template
                        .spec
                        .as_ref()
                        .ok_or_else(|| KubeApiError::missing_field(&resource, ".spec.template.spec"))?
                        .containers
                        .iter()
                        .find(|c| c.name == *container)
                        .ok_or_else(|| KubeApiError::invalid_state(&resource, format_args!("specified pod template does not contain target container `{container}`")))?;
                }

                Ok(ResolvedTarget::Deployment(ResolvedResource {
                    resource,
                    container,
                }))
            }

            ResolvedTarget::Pod(ResolvedResource {
                resource,
                container,
            }) => {
                let _ = RuntimeData::from_pod(&resource, container.as_deref())?;
                Ok(ResolvedTarget::Pod(ResolvedResource {
                    resource,
                    container,
                }))
            }

            ResolvedTarget::Rollout(ResolvedResource {
                resource,
                container,
            }) => {
                let available = resource
                    .status
                    .as_ref()
                    .ok_or_else(|| KubeApiError::missing_field(&resource, ".status"))?
                    .available_replicas
                    .unwrap_or_default(); // Field can be missing when there are no replicas

                if available <= 0 {
                    return Err(KubeApiError::invalid_state(
                        &resource,
                        "no available replicas",
                    ));
                }

                let pod_template = resource.get_pod_template(client).await?;

                if let Some(container) = &container {
                    // verify that the container exists
                    pod_template.spec.as_ref().ok_or_else(|| KubeApiError::invalid_state(&resource, "specified pod template is missing field `.spec`"))?
                    .containers
                                .iter()
                                .find(|c| c.name == *container)
                                .ok_or_else(|| KubeApiError::invalid_state(&resource, format_args!("specified pod template does not contain target container `{container}`")))?;
                }

                Ok(ResolvedTarget::Rollout(ResolvedResource {
                    resource,
                    container,
                }))
            }

            ResolvedTarget::Job(..) => {
                return Err(KubeApiError::requires_copy::<Job>());
            }

            ResolvedTarget::CronJob(..) => {
                return Err(KubeApiError::requires_copy::<CronJob>());
            }

            ResolvedTarget::StatefulSet(ResolvedResource {
                resource,
                container,
            }) => {
                let available = resource
                    .status
                    .as_ref()
                    .ok_or_else(|| KubeApiError::missing_field(&resource, ".status"))?
                    .available_replicas
                    .unwrap_or_default(); // Field can be missing when there are no replicas

                if available <= 0 {
                    return Err(KubeApiError::invalid_state(
                        &resource,
                        "no available replicas",
                    ));
                }

                if let Some(container) = &container {
                    // verify that the container exists
                    resource
                        .spec
                        .as_ref()
                        .ok_or_else(|| KubeApiError::missing_field(&resource, ".spec"))?
                        .template
                        .spec
                        .as_ref()
                        .ok_or_else(|| KubeApiError::missing_field(&resource, ".spec.template.spec"))?
                        .containers
                        .iter()
                        .find(|c| c.name == *container)
                        .ok_or_else(|| KubeApiError::invalid_state(&resource, format_args!("specified pod template does not contain target container `{container}`")))?;
                }

                Ok(ResolvedTarget::StatefulSet(ResolvedResource {
                    resource,
                    container,
                }))
            }

            ResolvedTarget::Service(ResolvedResource {
                resource,
                container,
            }) => {
                let pods = ResolvedResource::<Service>::get_pods(&resource, client).await?;

                if pods.is_empty() {
                    return Err(KubeApiError::invalid_state(
                        &resource,
                        "no pods matching the labels were found",
                    ));
                }

                if let Some(container) = &container {
                    let exists_in_a_pod = pods
                        .iter()
                        .flat_map(|pod| pod.spec.as_ref())
                        .flat_map(|spec| &spec.containers)
                        .any(|found_container| found_container.name == *container);
                    if !exists_in_a_pod {
                        return Err(KubeApiError::invalid_state(
                            &resource,
                            format_args!("none of the pods that match the labels contain the target container `{container}`"
                        )));
                    }
                }

                Ok(ResolvedTarget::Service(ResolvedResource {
                    resource,
                    container,
                }))
            }

            ResolvedTarget::Targetless(namespace) => {
                // no check needed here
                Ok(ResolvedTarget::Targetless(namespace))
            }
        }
    }
}

impl ResolvedTarget<true> {
    pub fn connect_url(
        &self,
        use_proxy: bool,
        concurrent_steal: ConcurrentSteal,
        api_version: &str,
        plural: &str,
        url_path: &str,
    ) -> String {
        let name = self.urlfied_name();
        let namespace = self.namespace().unwrap_or("default");

        if use_proxy {
            format!("/apis/{api_version}/proxy/namespaces/{namespace}/{plural}/{name}?on_concurrent_steal={concurrent_steal}&connect=true")
        } else {
            format!("{url_path}/{name}?on_concurrent_steal={concurrent_steal}&connect=true")
        }
    }

    pub fn urlfied_name(&self) -> String {
        let mut url = self.type_().to_string();

        if let Some(target_name) = self.name() {
            url.push_str(&format!(".{target_name}"));
        }

        if let Some(container) = self.container() {
            url.push_str(&format!(".container.{container}"));
        }

        url
    }
}
