#![cfg(test)]

mod init_containers_tests {
    use std::time::Duration;

    use rstest::*;

    use crate::utils::{kube_client, kube_service::KubeService, run_command::run_exec_with_target};

    /// Creates a pod with a native sidecar (init container with restartPolicy: Always).
    /// Native sidecars are a Kubernetes 1.28+ feature that allows init containers to run
    /// alongside regular containers throughout the pod's lifecycle.
    #[fixture]
    pub async fn pod_with_native_sidecar(#[future] kube_client: kube::Client) -> KubeService {
        use k8s_openapi::api::core::v1::Pod;
        use kube::Api;
        use serde_json::json;

        use crate::utils::{
            random_string, resource_guard::ResourceGuard, services::TestWorkloadType, watch,
            PRESERVE_FAILED_ENV_NAME, TEST_RESOURCE_LABEL,
        };

        let client = kube_client.await;
        let namespace = "default";
        let pod_name = format!("test-sidecar-{}", random_string());

        let pod_api: Api<Pod> = Api::namespaced(client.clone(), namespace);

        // Create a pod with:
        // - A main application container
        // - A native sidecar (init container with restartPolicy: Always)
        let pod_spec = json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": pod_name,
                "namespace": &namespace,
                "labels": {
                    TEST_RESOURCE_LABEL.0: "true",
                }
            },
            "spec": {
                "initContainers": [{
                    "name": "sidecar",
                    "image": "ghcr.io/metalbear-co/mirrord-pytest:latest",
                    "restartPolicy": "Always",
                    "command": ["python", "-u", "-m", "http.server", "9090"],
                    "env": [{
                        "name": "CONTAINER_NAME",
                        "value": "sidecar"
                    }]
                }],
                "containers": [{
                    "name": "main",
                    "image": "ghcr.io/metalbear-co/mirrord-pytest:latest",
                    "command": ["python", "-u", "-m", "http.server", "80"],
                    "env": [{
                        "name": "CONTAINER_NAME",
                        "value": "main"
                    }]
                }]
            }
        });

        let pod: Pod = serde_json::from_value(pod_spec).expect("Failed to create pod spec");
        let delete_on_fail = std::env::var_os(PRESERVE_FAILED_ENV_NAME).is_none();
        let guard = ResourceGuard::create(pod_api.clone(), &pod, delete_on_fail)
            .await
            .expect("Failed to create pod")
            .0;

        watch::wait_until_pod_ready(&pod_name, namespace, client).await;

        KubeService {
            name: pod_name.clone(),
            namespace: namespace.to_owned(),
            guards: vec![guard],
            pod_name,
            workload_type: TestWorkloadType::Deployment,
        }
    }

    /// Test that we can target a native sidecar init container by name
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(120))]
    pub async fn target_native_sidecar(#[future] pod_with_native_sidecar: KubeService) {
        let service = pod_with_native_sidecar.await;

        // Target the sidecar init container specifically
        let target = format!("pod/{}/container/sidecar", service.pod_name);

        let mut process = run_exec_with_target(
            ["bash", "-c", "test \"$CONTAINER_NAME\" == 'sidecar'"]
                .map(String::from)
                .to_vec(),
            &target,
            Some(&service.namespace),
            None,
            None,
        )
        .await;

        process.wait_assert_success().await;
    }

    /// Test that we target a main container by default
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(120))]
    pub async fn main_container_by_default(#[future] pod_with_native_sidecar: KubeService) {
        let service = pod_with_native_sidecar.await;

        // When targeting just the pod without specifying container,
        // mirrord should see both the main container and the sidecar
        // and pick the first non-init container (main)
        let target = format!("pod/{}", service.pod_name);

        let mut process = run_exec_with_target(
            ["bash", "-c", "test \"$CONTAINER_NAME\" == 'main'"]
                .map(String::from)
                .to_vec(),
            &target,
            Some(&service.namespace),
            None,
            None,
        )
        .await;

        process.wait_assert_success().await;
    }
}
