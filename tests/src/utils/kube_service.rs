#![allow(dead_code)]
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Service};

use super::resource_guard::ResourceGuard;
use crate::utils::CONTAINER_NAME;

/// A service deployed to the kubernetes cluster.
///
/// Service is meant as in "Microservice", not as in the Kubernetes resource called Service.
/// This includes a Deployment resource, a Service resource and optionally a Namespace.
pub struct KubeService {
    pub name: String,
    pub namespace: String,
    pub service: Service,
    pub deployment: Deployment,
    pub(crate) guards: Vec<ResourceGuard>,
    pub(crate) namespace_guard: Option<ResourceGuard>,
    pub pod_name: String,
}

impl KubeService {
    pub fn deployment_target(&self) -> String {
        format!("deployment/{}", self.name)
    }

    pub fn pod_container_target(&self) -> String {
        format!("pod/{}/container/{CONTAINER_NAME}", self.pod_name)
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
