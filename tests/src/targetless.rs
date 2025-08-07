#[cfg(test)]
/// Test the targetless execution mode, where an independent agent is spawned - not targeting any
/// existing pod/container/deployment.
mod targetless_tests {
    use std::{convert::Infallible, time::Duration};

    use k8s_openapi::{
        api::{core::v1::Pod, scheduling::v1::PriorityClass},
        apimachinery::pkg::apis::meta::v1::ObjectMeta,
    };
    use kube::{api::ListParams, Api, Client};
    use rstest::rstest;
    use tempfile::NamedTempFile;

    use crate::utils::{
        application::Application, kube_client, operator_installed, random_string,
        resource_guard::ResourceGuard,
    };

    /// `mirrord exec` a program that connects to the kubernetes api service by its internal name
    /// from within the cluster, without specifying a target. The http request gets an 403 error
    /// because we do not authenticate, but getting this response means we successfully resolved
    /// dns inside the cluster and connected to a ClusterIP (not reachable from outside of the
    /// cluster).
    ///
    /// Running this test on a cluster that does not have any pods, also proves that we don't use
    /// any existing pod and the agent pod is completely independent.
    #[cfg_attr(target_os = "windows", ignore)]
    #[cfg_attr(not(feature = "targetless"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(30))]
    pub async fn connect_to_kubernetes_api_service_with_targetless_agent() {
        let app = Application::CurlToKubeApi;
        let mut process = app.run_targetless(None, None, None).await;
        let res = process.wait().await;
        assert!(res.success());
        let stdout = process.get_stdout().await;
        assert!(stdout.contains(r#""apiVersion": "v1""#))
    }

    /// Test spawning a targetless agent pod with a given priority class.
    #[cfg_attr(target_os = "windows", ignore)]
    #[cfg_attr(not(feature = "targetless"), ignore)]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn targetless_agent_with_priority_class(#[future] kube_client: Client) {
        let kube_client = kube_client.await;
        if operator_installed(&kube_client).await.unwrap() {
            return;
        }
        // create a priority class
        let priority_class = PriorityClass {
            metadata: ObjectMeta {
                name: Some(format!("test-priorityclass-{}", random_string())),
                ..Default::default()
            },
            value: 1024,
            ..Default::default()
        };
        let (_guard, priority_class) = ResourceGuard::create(
            Api::<PriorityClass>::all(kube_client.clone()),
            &priority_class,
            true,
        )
        .await
        .unwrap();
        let priority_class_name = priority_class.metadata.name.unwrap();

        // specify priority class name in agent config
        let mut config_file = NamedTempFile::with_suffix(".json").unwrap();
        serde_json::to_writer_pretty(
            config_file.as_file_mut(),
            &serde_json::json!({
                "agent": {
                    "priority_class": priority_class_name,
                    "labels": {
                        "test-id": priority_class_name,
                    }
                }
            }),
        )
        .unwrap();

        // run a test app with the config
        let test_process = Application::PythonFlaskHTTP
            .run_targetless(
                None,
                Some(vec!["-f", config_file.path().to_str().unwrap()]),
                None,
            )
            .await;

        // assert agent priority class
        let pods: Api<Pod> = Api::namespaced(kube_client.clone(), "default");
        let lp = ListParams::default().labels(&format!("test-id={}", priority_class_name));
        // Wait for the agent pod to be ready and find it
        let agent_pod = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let pod_list = pods.list(&lp).await.unwrap();
                if let Some(agent) = pod_list.items.iter().find(|p| {
                    p.metadata
                        .name
                        .as_deref()
                        .unwrap_or("")
                        .starts_with("mirrord-agent")
                }) {
                    break Ok::<Pod, Infallible>(agent.clone());
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        })
        .await
        .expect("Timeout while waiting for agent pod")
        .unwrap();

        let spec = agent_pod.spec.expect("Agent pod spec should exist");
        assert_eq!(
            spec.priority_class_name.as_ref().unwrap(),
            &priority_class_name,
            "Agent pod did not have the expected priority class name"
        );

        tokio::time::sleep(Duration::from_secs(1)).await;
        drop(test_process);
    }
}
