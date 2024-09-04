use std::fmt;

use async_stream::stream;
use futures::{stream, Stream, StreamExt, TryStreamExt};
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, StatefulSet},
        batch::v1::{CronJob, Job},
        core::v1::Pod,
    },
    Metadata, NamespaceResourceScope,
};
use kube::{api::ListParams, Resource};
use serde::de;

use crate::{
    api::{
        container::SKIP_NAMES,
        kubernetes::{get_k8s_resource_api, rollout::Rollout},
    },
    error::Result,
};

pub struct KubeResourceSeeker<'a> {
    pub client: &'a kube::Client,
    pub namespace: Option<&'a str>,
}

impl KubeResourceSeeker<'_> {
    /// Returns all resource types that don't require the operator to operate ie. [`Pod`],
    /// [`Deployment`] and [`Rollout`]
    pub async fn all_open_source(&self) -> Result<Vec<String>> {
        let pods = self.pods().await?;
        let deployments = self.deployments().await?;
        let rollouts = self.simple_list_resource::<Rollout>("rollout").await?;

        Ok(pods
            .into_iter()
            .chain(deployments)
            .chain(rollouts)
            .collect())
    }

    /// Returns all resource types ie. [`Pod`], [`Deployment`], [`Rollout`], [`Job`], [`CronJob`],
    /// and [`StatefulSet`]
    pub async fn all(&self) -> Result<Vec<String>> {
        let pods = self.pods().await?;
        let deployments = self
            .simple_list_resource::<Deployment>("deployment")
            .await?;
        let rollouts = self.simple_list_resource::<Rollout>("rollout").await?;
        let jobs = self.simple_list_resource::<Job>("job").await?;
        let cronjobs = self.simple_list_resource::<CronJob>("cronjob").await?;
        let statefulsets = self
            .simple_list_resource::<StatefulSet>("statefulset")
            .await?;

        Ok(pods
            .into_iter()
            .chain(deployments)
            .chain(rollouts)
            .chain(jobs)
            .chain(cronjobs)
            .chain(statefulsets)
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

        self.list_resource::<Pod>(Some("status.phase=Running"))
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

        self.list_resource::<Deployment>(None)
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
    }

    /// Helper to get the list of a resource type ([`Pod`], [`Deployment`], [`Rollout`], [`Job`],
    /// [`CronJob`], [`StatefulSet`]) through the kube api.
    fn list_resource<'s, R>(
        &self,
        field_selector: Option<&'s str>,
    ) -> impl Stream<Item = Result<R>> + 's
    where
        R: Clone + fmt::Debug + for<'de> de::Deserialize<'de> + 's,
        R: Resource<DynamicType = (), Scope = NamespaceResourceScope>,
    {
        let Self { client, namespace } = self;
        let resource_api = get_k8s_resource_api::<R>(client, *namespace);

        stream! {
            let mut params =  ListParams {
                label_selector: Some("app!=mirrord,!operator.metalbear.co/owner".to_string()),
                field_selector: field_selector.map(ToString::to_string),
                ..Default::default()
            };

            loop {
                let resource = resource_api.list(&params).await?;

                for resource in resource.items {
                    yield Ok(resource);
                }

                if let Some(continue_token) = resource.metadata.continue_ {
                    params = params.continue_token(&continue_token);
                } else {
                    break;
                }
            }
        }
    }

    async fn simple_list_resource<'s, R>(&self, prefix: &'s str) -> Result<Vec<String>>
    where
        R: Clone + fmt::Debug + for<'de> de::Deserialize<'de>,
        R: Resource<DynamicType = (), Scope = NamespaceResourceScope> + Metadata,
    {
        self.list_resource::<R>(None)
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
    }
}
