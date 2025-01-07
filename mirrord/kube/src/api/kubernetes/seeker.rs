use std::fmt;

use futures::{stream, Stream, StreamExt, TryStreamExt};
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, StatefulSet},
        batch::v1::{CronJob, Job},
        core::v1::Pod,
    },
    ClusterResourceScope, Metadata, NamespaceResourceScope,
};
use kube::{api::ListParams, Api, Resource};
use serde::de::{self, DeserializeOwned};

use crate::{
    api::{
        container::SKIP_NAMES,
        kubernetes::{get_k8s_resource_api, rollout::Rollout},
    },
    error::{KubeApiError, Result},
};

pub struct KubeResourceSeeker<'a> {
    pub client: &'a kube::Client,
    pub namespace: Option<&'a str>,
}

impl KubeResourceSeeker<'_> {
    /// Returns all resource types that don't require the operator to operate ie. [`Pod`],
    /// [`Deployment`] and [`Rollout`]
    pub async fn all_open_source(&self) -> Result<Vec<String>> {
        let (pods, deployments, rollouts) = tokio::try_join!(
            self.pods(),
            self.deployments(),
            self.simple_list_resource::<Rollout>("rollout")
        )?;

        Ok(pods
            .into_iter()
            .chain(deployments)
            .chain(rollouts)
            .collect())
    }

    /// Returns all resource types ie. [`Pod`], [`Deployment`], [`Rollout`], [`Job`], [`CronJob`],
    /// and [`StatefulSet`]
    pub async fn all(&self) -> Result<Vec<String>> {
        let (pods, deployments, rollouts, jobs, cronjobs, statefulsets) = tokio::try_join!(
            self.pods(),
            self.simple_list_resource::<Deployment>("deployment"),
            self.simple_list_resource::<Rollout>("rollout"),
            self.simple_list_resource::<Job>("job"),
            self.simple_list_resource::<CronJob>("cronjob"),
            self.simple_list_resource::<StatefulSet>("statefulset"),
        )?;

        Ok(deployments
            .into_iter()
            .chain(rollouts)
            .chain(statefulsets)
            .chain(cronjobs)
            .chain(jobs)
            .chain(pods)
            .collect())
    }

    /// Returns a list of (pod name, [container names]) pairs, filtering out mesh side cars
    /// as well as any pods which are not ready or have crashed.
    async fn pods(&self) -> Result<Vec<String>> {
        fn check_pod_status(pod: &Pod) -> bool {
            pod.status
                .as_ref()
                .and_then(|status| status.conditions.as_ref())
                .map(|conditions| {
                    // filter out pods without the Ready condition
                    conditions
                        .iter()
                        .any(|condition| condition.type_ == "Ready" && condition.status == "True")
                })
                .unwrap_or(false)
        }

        fn create_pod_container_map(pod: Pod) -> Option<(String, Vec<String>)> {
            let name = pod.metadata.name.clone()?;
            let containers = pod
                .spec
                .as_ref()?
                .containers
                .iter()
                .filter(|&container| (!SKIP_NAMES.contains(container.name.as_str())))
                .map(|container| container.name.clone())
                .collect();

            Some((name, containers))
        }

        self.list_all_namespaced(Some("status.phase=Running"))
            .try_filter(|pod| std::future::ready(check_pod_status(pod)))
            .try_filter_map(|pod| std::future::ready(Ok(create_pod_container_map(pod))))
            .map_ok(|(pod, containers)| {
                stream::iter(if containers.len() == 1 {
                    vec![Ok(format!("pod/{pod}"))]
                } else {
                    containers
                        .iter()
                        .map(move |container| Ok(format!("pod/{pod}/container/{container}")))
                        .collect()
                })
            })
            .try_flatten()
            .try_collect()
            .await
            .map_err(KubeApiError::KubeError)
    }

    /// The list of deployments that have at least 1 `Replicas` and a deployment name.
    async fn deployments(&self) -> Result<Vec<String>> {
        fn check_deployment_replicas(deployment: &Deployment) -> bool {
            deployment
                .status
                .as_ref()
                .map(|status| status.available_replicas >= Some(1))
                .unwrap_or(false)
        }

        self.list_all_namespaced::<Deployment>(None)
            .filter(|response| std::future::ready(response.is_ok()))
            .try_filter(|deployment| std::future::ready(check_deployment_replicas(deployment)))
            .try_filter_map(|deployment| {
                std::future::ready(Ok(deployment
                    .metadata
                    .name
                    .map(|name| format!("deployment/{name}"))))
            })
            .try_collect()
            .await
            .map_err(From::from)
    }

    async fn simple_list_resource<'s, R>(&self, prefix: &'s str) -> Result<Vec<String>>
    where
        R: 'static
            + Clone
            + fmt::Debug
            + for<'de> de::Deserialize<'de>
            + Resource<DynamicType = (), Scope = NamespaceResourceScope>
            + Metadata
            + Send,
    {
        self.list_all_namespaced::<R>(None)
            .filter(|response| std::future::ready(response.is_ok()))
            .try_filter_map(|rollout| {
                std::future::ready(Ok(rollout
                    .meta()
                    .name
                    .as_ref()
                    .map(|name| format!("{prefix}/{name}"))))
            })
            .try_collect()
            .await
            .map_err(From::from)
    }

    /// Prepares [`ListParams`] that:
    /// 1. Excludes our own resources
    /// 2. Adds a limit for item count in a response
    fn make_list_params(field_selector: Option<&str>) -> ListParams {
        ListParams {
            label_selector: Some("app!=mirrord,!operator.metalbear.co/owner".to_string()),
            field_selector: field_selector.map(ToString::to_string),
            limit: Some(500),
            ..Default::default()
        }
    }

    /// Returns a [`Stream`] of all objects in this [`KubeResourceSeeker`]'s namespace.
    ///
    /// 1. `field_selector` can be used for filtering.
    /// 2. Our own resources are excluded.
    pub fn list_all_namespaced<R>(
        &self,
        field_selector: Option<&str>,
    ) -> impl 'static + Stream<Item = kube::Result<R>> + Send
    where
        R: 'static
            + Resource<DynamicType = (), Scope = NamespaceResourceScope>
            + fmt::Debug
            + Clone
            + DeserializeOwned
            + Send,
    {
        let api = get_k8s_resource_api(self.client, self.namespace);
        let mut params = Self::make_list_params(field_selector);

        async_stream::stream! {
            loop {
                let response = api.list(&params).await?;

                for resource in response.items {
                    yield Ok(resource);
                }

                let continue_token = response.metadata.continue_.unwrap_or_default();
                if continue_token.is_empty() {
                    break;
                }
                params.continue_token.replace(continue_token);
            }
        }
    }

    /// Returns a [`Stream`] of all objects in the cluster.
    ///
    /// 1. `field_selector` can be used for filtering.
    /// 2. Our own resources are excluded.
    pub fn list_all_clusterwide<R>(
        &self,
        field_selector: Option<&str>,
    ) -> impl 'static + Stream<Item = kube::Result<R>> + Send
    where
        R: 'static
            + Resource<DynamicType = (), Scope = ClusterResourceScope>
            + fmt::Debug
            + Clone
            + DeserializeOwned
            + Send,
    {
        let api = Api::all(self.client.clone());
        let mut params = Self::make_list_params(field_selector);

        async_stream::stream! {
            loop {
                let response = api.list(&params).await?;

                for resource in response.items {
                    yield Ok(resource);
                }

                let continue_token = response.metadata.continue_.unwrap_or_default();
                if continue_token.is_empty() {
                    break;
                }
                params.continue_token.replace(continue_token);
            }
        }
    }
}
