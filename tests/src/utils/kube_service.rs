#![allow(dead_code)]
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Service};
use mirrord_kube::api::kubernetes::rollout::Rollout;

use super::resource_guard::ResourceGuard;
use crate::utils::{cluster_resource::argo_rollout_from_deployment, watch, CONTAINER_NAME};

/// A service deployed to the kubernetes cluster.
///
/// Service is meant as in "Microservice", not as in the Kubernetes resource called Service.
/// This includes a Deployment resource, a Service resource and optionally a Namespace.
pub struct KubeService {
    pub name: String,
    pub namespace: String,
    pub service: Service,
    pub deployment: Deployment,
    pub rollout: Option<Rollout>,
    pub guards: Vec<ResourceGuard>,
    pub namespace_guard: Option<ResourceGuard>,
    pub pod_name: String,
}

impl KubeService {
    pub fn deployment_target(&self) -> String {
        format!("deployment/{}", self.name)
    }

    pub fn pod_container_target(&self) -> String {
        format!("pod/{}/container/{CONTAINER_NAME}", self.pod_name)
    }

    pub fn rollout_target(&self) -> String {
        if self.rollout.is_none() {
            panic!("Rollout is not enabled for this service! Don't you mean to use a deployment?");
        }

        format!("rollout/{}", self.name)
    }

    pub fn rollout_or_deployment_target(&self) -> String {
        if self.rollout.is_some() {
            self.rollout_target()
        } else {
            self.deployment_target()
        }
    }

    /// Create a rollout, a `ResourceGuard` that deletes it, and wait for the rollout to be
    /// available.
    /// Add the rollout to this service
    pub async fn add_rollout(&mut self, kube_client: kube::Client, delete_after_fail: bool) {
        let rollout = argo_rollout_from_deployment(&self.name, &self.deployment);
        let rollout_api: kube::Api<Rollout> =
            kube::Api::namespaced(kube_client.clone(), &self.namespace);
        let (rollout_guard, rollout) =
            ResourceGuard::create(rollout_api, &rollout, delete_after_fail)
                .await
                .unwrap_or_else(|err| {
                    panic!(
                        "Failed to create rollout guard! Error: \n{err:?}\nRollout:\n{}",
                        serde_json::to_string_pretty(&rollout).unwrap()
                    )
                });
        println!(
            "Created rollout\n{}",
            serde_json::to_string_pretty(&rollout).unwrap()
        );

        // Wait for the rollout to have at least 1 available replica
        watch::wait_until_rollout_available(&self.name, &self.namespace, 1, kube_client).await;

        self.rollout = Some(rollout);
        self.guards.push(rollout_guard);
    }
}

impl Drop for KubeService {
    fn drop(&mut self) {
        let mut deleters = self
            .guards
            .iter_mut()
            .map(ResourceGuard::take_deleter)
            .collect::<Vec<_>>();

        deleters.push(
            self.namespace_guard
                .as_mut()
                .and_then(ResourceGuard::take_deleter),
        );

        let deleters = deleters.into_iter().flatten().collect::<Vec<_>>();

        if deleters.is_empty() {
            return;
        }

        let _ = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to create tokio runtime")
                .block_on(futures::future::join_all(deleters));
        })
        .join();
    }
}
