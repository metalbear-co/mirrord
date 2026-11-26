#![allow(dead_code)]

use std::{ops::Not, time::Duration};

use k8s_openapi::api::core::v1::Pod;
use kube::{runtime::watcher::Config, Api, Client};

use super::resource_guard::ResourceGuard;
use crate::utils::{services::TestWorkloadType, watch::Watcher, CONTAINER_NAME};

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
            panic!(
                "Stateful set is not enabled for this service, did you mean to use a deployment?"
            );
        }

        format!("statefulset/{}", self.name)
    }

    /// Waits until all job agents of [`Self::pod_container_target`] have exited.
    ///
    /// Use it between two sessions on this target, because an agent holds the iptables lock
    /// until it exits.
    pub async fn wait_for_agents_to_exit(&self, client: Client) {
        let pods = Api::<Pod>::namespaced(client, &self.namespace);

        let target = pods.get(&self.pod_name).await.unwrap();
        let container_id = target
            .status
            .and_then(|status| status.container_statuses)
            .unwrap_or_default()
            .into_iter()
            .find(|status| status.name == CONTAINER_NAME)
            .and_then(|status| status.container_id)
            .expect("target container has no container ID");
        // The agent gets the ID without the runtime prefix, e.g. `containerd://`.
        let container_id = container_id
            .rsplit("://")
            .next()
            .unwrap_or(&container_id)
            .to_owned();

        let mut watcher = Watcher::new(
            pods,
            Config {
                label_selector: Some("app=mirrord".to_owned()),
                ..Default::default()
            },
            move |pods| {
                pods.values()
                    .filter(|pod| {
                        pod.spec
                            .iter()
                            .flat_map(|spec| &spec.containers)
                            .any(|container| {
                                container
                                    .args
                                    .iter()
                                    .flatten()
                                    .any(|arg| *arg == container_id)
                            })
                    })
                    .all(|pod| {
                        let phase = pod
                            .status
                            .as_ref()
                            .and_then(|status| status.phase.as_deref());
                        matches!(phase, Some("Succeeded" | "Failed"))
                    })
            },
        );

        tokio::time::timeout(Duration::from_secs(60), watcher.run())
            .await
            .expect("agents of the target did not exit in time");
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
