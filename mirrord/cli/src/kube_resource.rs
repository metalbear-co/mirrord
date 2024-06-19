use std::collections::HashMap;

use k8s_openapi::{
    api::{apps::v1::Deployment, core::v1::Pod},
    Metadata, NamespaceResourceScope,
};
use kube::api::ListParams;
use mirrord_kube::api::{
    container::SKIP_NAMES,
    kubernetes::{get_k8s_resource_api, rollout::Rollout},
};
use serde::de::DeserializeOwned;

pub(super) struct KubeResourceSeeker<'a> {
    pub(super) client: &'a kube::Client,
    pub(super) namespace: Option<&'a str>,
}

impl KubeResourceSeeker<'_> {
    /// Returns a tuple with:
    /// 1. list of pods according to [`Self::pods`];
    /// 2. list of deployments according to [`Self::deployments`];
    /// 2. list of rollouts according to [`Self::rollouts`];
    pub(super) async fn all_open_source(
        &self,
    ) -> (
        HashMap<String, Vec<String>>,
        impl Iterator<Item = String>,
        impl Iterator<Item = String>,
    ) {
        futures::join!(self.pods(), self.deployments(), self.rollouts())
    }

    /// Returns a list of (pod name, [container names]) pairs, filtering out mesh side cars
    /// as well as any pods which are not ready or have crashed.
    async fn pods(&self) -> HashMap<String, Vec<String>> {
        let pods = self
            .list_resource::<Pod>(Some("status.phase=Running"))
            .await
            .into_iter()
            .filter(|pod| {
                pod.status
                    .as_ref()
                    .and_then(|status| status.conditions.as_ref())
                    .map(|conditions| {
                        // filter out pods without the Ready condition
                        conditions.iter().any(|condition| {
                            condition.type_ == "Ready" && condition.status == "True"
                        })
                    })
                    .unwrap_or(false)
            });

        // convert pods to (name, container names) pairs
        pods.filter_map(|pod| {
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
        })
        .collect()
    }

    /// The list of deployments that have at least 1 `Replicas` and a deployment name.
    async fn deployments(&self) -> impl Iterator<Item = String> {
        self.list_resource::<Deployment>(None)
            .await
            .into_iter()
            .filter(|deployment| {
                deployment
                    .status
                    .as_ref()
                    .map(|status| status.available_replicas >= Some(1))
                    .unwrap_or(false)
            })
            .filter_map(|deployment| deployment.metadata.name)
    }

    /// The list of rollouts that have a name.
    async fn rollouts(&self) -> impl Iterator<Item = String> {
        self.list_resource::<Rollout>(None)
            .await
            .into_iter()
            .filter_map(|rollout| rollout.metadata().name.clone())
    }

    /// Helper to get the list of a resource type ([`Pod`], [`Deployment`], [`Rollout`])
    /// through the kube api.
    async fn list_resource<K>(&self, field_selector: Option<&str>) -> Vec<K>
    where
        K: kube::Resource<Scope = NamespaceResourceScope>,
        <K as kube::Resource>::DynamicType: Default,
        K: Clone + DeserializeOwned + std::fmt::Debug,
    {
        let Self { client, namespace } = self;

        // Set up filters on the K8s resources returned - in this case, excluding the agent
        // resources and then applying any provided field-based filter conditions.
        let params = ListParams {
            label_selector: Some("app!=mirrord".to_string()),
            field_selector: field_selector.map(ToString::to_string),
            ..Default::default()
        };

        get_k8s_resource_api(client, *namespace)
            .list(&params)
            .await
            .map(|resources| resources.items)
            .unwrap_or_default()
    }
}
