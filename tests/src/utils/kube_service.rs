#![allow(dead_code)]

use std::ops::Not;

use super::resource_guard::ResourceGuard;
use crate::utils::{services::TestWorkloadType, CONTAINER_NAME};

/// A service deployed to the kubernetes cluster.
///
/// Service is meant as in "Microservice", not as in the Kubernetes resource called Service.
/// This includes a Deployment resource, a Service resource and optionally a Namespace.
pub struct KubeService {
    pub name: String,
    pub namespace: String,
    pub guards: Vec<ResourceGuard>,
    pub pod_name: String,
    pub workload_type: TestWorkloadType,
}

impl KubeService {
    pub fn deployment_target(&self) -> String {
        format!("deployment/{}", self.name)
    }

    pub fn pod_container_target(&self) -> String {
        format!("pod/{}/container/{CONTAINER_NAME}", self.pod_name)
    }

    pub fn is_rollout(&self) -> bool {
        self.workload_type.is_rollout()
    }

    pub fn rollout_target(&self) -> String {
        if self.workload_type.is_rollout().not() {
            panic!("Rollout is not enabled for this service! Don't you mean to use a deployment?");
        }

        format!("rollout/{}", self.name)
    }

    pub fn rollout_or_deployment_target(&self) -> String {
        if self.workload_type.is_rollout() {
            self.rollout_target()
        } else {
            self.deployment_target()
        }
    }

    pub fn stateful_set_target(&self) -> String {
        if self.workload_type.is_stateful_set().not() {
            panic!("Stateful set is not enabled for this service, did you mean to use a deployment?");
        }

        format!("statefulset/{}", self.name)
    }
}

impl Drop for KubeService {
    fn drop(&mut self) {
        let deleters = self
            .guards
            .iter_mut()
            .map(ResourceGuard::take_deleter)
            .collect::<Vec<_>>();

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
