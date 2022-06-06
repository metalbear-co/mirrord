pub mod utils;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use k8s_openapi::api::{batch::v1::Job, core::v1::Pod};
    use kube::{api::ListParams, Api};
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, BufReader},
        time::{timeout, Duration},
    };

    use crate::utils::*;

    #[tokio::test]
    #[ignore]
    async fn test_complete_node_api() {
        _test_complete_node_api().await;
    }

    // actual test function used with "containerd", "docker" runtimes
    // starts the node(express.js) api server, sends four different requests, validates data,
    // stops the server and validates if the agent job and pod are deleted
    async fn _test_complete_node_api() {
        let client = setup_kube_client().await;

        let pod_namespace = "default";
        let env: HashMap<&str, &str> = HashMap::new(); // for adding more environment variables
        let mut server = test_server_init(&client, pod_namespace, env).await;

        let service_url = get_service_url(&client, pod_namespace).await.unwrap();

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
        let child_stdout = server.stdout.take().unwrap();

        // Note: to run a task in the background, don't await on it.
        // spawn returns a JoinHandle
        tokio::spawn(async move {
            loop {
                let mut error_stream = String::new();
                // the following is a blocking call, so we need to spawn a task to read the stderr
                stderr_reader.read_line(&mut error_stream).await.unwrap();
                // Todo(investigate): it looks like the task reaches the panic when not kept in a
                // loop i.e. the buffer reads an empty string:  `thread 'main'
                // panicked at 'Error: '`
                if !error_stream.is_empty() {
                    panic!("Error: {}", error_stream);
                }
            }
        });

        // since we are reading from the stdout, we could block at any point in case the server
        // does not write to its stdout, so we need a timeout here
        let validation_timeout = Duration::from_secs(20);
        timeout(
            validation_timeout,
            validate_requests(child_stdout, service_url.as_str()),
        )
        .await
        .unwrap();
        server.kill().await.unwrap();

        let jobs_api: Api<Job> = Api::namespaced(client.clone(), "default");
        let jobs = jobs_api.list(&ListParams::default()).await.unwrap();
        // assuming only one job is running
        // to make the tests parallel we need to figure a way to get the exact job name
        // when len() > 1
        assert_eq!(jobs.items.len(), 1);

        let pods_api: Api<Pod> = Api::namespaced(client.clone(), "default");
        let pods = pods_api.list(&ListParams::default()).await.unwrap();
        assert_eq!(pods.items.len(), 2);

        let cleanup_timeout = Duration::from_secs(35);
        timeout(
            cleanup_timeout,
            tokio::spawn(async move {
                // verify cleanup
                loop {
                    let updated_jobs = jobs_api.list(&ListParams::default()).await.unwrap();
                    let updated_pods = pods_api.list(&ListParams::default()).await.unwrap(); // only the nginx pod should exist
                    if updated_pods.items.len() == 1 && updated_jobs.items.is_empty() {
                        let nginx_pod = updated_pods.items[0].metadata.name.clone().unwrap();
                        assert!(nginx_pod.contains("nginx"));
                        break;
                    }
                }
            }),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    // we send a request to a different pod in the cluster (different namespace) and assert
    // that no operation is performed as specified in the request by the server
    // as the agent pod is impersonating the pod running in the default namespace
    async fn test_different_pod_in_cluster() {
        let client = setup_kube_client().await;

        let test_namespace = "test-namespace";
        let pod_namespace = "default";
        let env: HashMap<&str, &str> = HashMap::new();
        let mut server = test_server_init(&client, pod_namespace, env).await;

        create_namespace(&client, test_namespace).await;
        create_nginx_pod(&client, test_namespace).await;

        let service_url = get_service_url(&client, test_namespace).await.unwrap();

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
        tokio::spawn(async move {
            loop {
                let mut error_stream = String::new();
                stderr_reader.read_line(&mut error_stream).await.unwrap();
                if !error_stream.is_empty() {
                    panic!("Error: {}", error_stream);
                }
            }
        });

        let child_stdout = server.stdout.take().unwrap();
        let timeout_duration = Duration::from_secs(10);
        timeout(
            timeout_duration,
            validate_no_requests(child_stdout, service_url.as_str()),
        )
        .await
        .unwrap();

        server.kill().await.unwrap();
        delete_namespace(&client, test_namespace).await;
    }

    // agent namespace tests
    #[tokio::test]
    // creates a new k8s namespace, starts the API server with env:
    // MIRRORD_AGENT_NAMESPACE=namespace, asserts that the agent job and pod are created
    // validate data through requests to the API server
    async fn test_good_agent_namespace() {
        let client = setup_kube_client().await;

        let agent_namespace = "test-namespace-agent-good";
        let pod_namespace = "default";
        let env = HashMap::from([("MIRRORD_AGENT_NAMESPACE", agent_namespace)]);
        let mut server = test_server_init(&client, pod_namespace, env).await;

        create_namespace(&client, agent_namespace).await;

        let service_url = get_service_url(&client, "default").await.unwrap();

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
        let child_stdout = server.stdout.take().unwrap();

        tokio::spawn(async move {
            loop {
                let mut error_stream = String::new();
                stderr_reader.read_line(&mut error_stream).await.unwrap();
                if !error_stream.is_empty() {
                    panic!("Error: {}", error_stream);
                }
            }
        });

        let validation_timeout = Duration::from_secs(20);
        timeout(
            validation_timeout,
            validate_requests(child_stdout, service_url.as_str()),
        )
        .await
        .unwrap();
        server.kill().await.unwrap();

        let jobs_api: Api<Job> = Api::namespaced(client.clone(), agent_namespace);
        let jobs = jobs_api.list(&ListParams::default()).await.unwrap();
        assert_eq!(jobs.items.len(), 1);

        let pods_api: Api<Pod> = Api::namespaced(client.clone(), agent_namespace);
        let pods = pods_api.list(&ListParams::default()).await.unwrap();
        assert_eq!(pods.items.len(), 1);
        delete_namespace(&client, agent_namespace).await;
    }

    #[tokio::test]
    // starts the API server with env: MIRRORD_AGENT_NAMESPACE=namespace (nonexistent),
    // asserts the process crashes: "NotFound" as the namespace does not exist
    async fn test_nonexistent_agent_namespace() {
        let client = setup_kube_client().await;
        let agent_namespace = "nonexistent-namespace";
        let pod_namespace = "default";
        let env = HashMap::from([("MIRRORD_AGENT_NAMESPACE", agent_namespace)]);
        let mut server = test_server_init(&client, pod_namespace, env).await;

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());

        let timeout_duration = Duration::from_secs(5);
        timeout(
            timeout_duration,
            tokio::spawn(async move {
                loop {
                    let mut error_stream = String::new();
                    stderr_reader
                        .read_to_string(&mut error_stream)
                        .await
                        .unwrap();
                    if !error_stream.is_empty() {
                        assert!(error_stream.contains("NotFound")); //Todo: fix this when unwraps are removed in pod_api.rs
                        break;
                    }
                }
            }),
        )
        .await
        .unwrap()
        .unwrap();
    }

    // pod namespace tests
    #[tokio::test]
    // creates a new k8s namespace, starts the API server with env:
    // MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE=namespace, validates data sent through
    // requests
    async fn test_good_pod_namespace() {
        let client = setup_kube_client().await;

        let pod_namespace = "test-pod-namespace";
        create_namespace(&client, pod_namespace).await;
        create_nginx_pod(&client, pod_namespace).await;

        let env = HashMap::from([("MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE", pod_namespace)]);
        let mut server = test_server_init(&client, pod_namespace, env).await;

        let service_url = get_service_url(&client, pod_namespace).await.unwrap();

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
        tokio::spawn(async move {
            loop {
                let mut error_stream = String::new();
                stderr_reader.read_line(&mut error_stream).await.unwrap();
                if !error_stream.is_empty() {
                    panic!("Error: {}", error_stream);
                }
            }
        });

        let child_stdout = server.stdout.take().unwrap();

        let validation_timeout = Duration::from_secs(20);
        timeout(
            validation_timeout,
            validate_requests(child_stdout, service_url.as_str()),
        )
        .await
        .unwrap();
        server.kill().await.unwrap();
        delete_namespace(&client, pod_namespace).await;
    }

    #[tokio::test]
    // starts the API server with env: MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE=namespace
    // (nonexistent), asserts the process crashes: "NotFound" as the namespace does not
    // exist
    async fn test_bad_pod_namespace() {
        let client = setup_kube_client().await;

        let pod_namespace = "default";
        let env = HashMap::from([(
            "MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE",
            "nonexistent-namespace",
        )]);
        let mut server = test_server_init(&client, pod_namespace, env).await;

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
        let timeout_duration = Duration::from_secs(5);
        timeout(
            timeout_duration,
            tokio::spawn(async move {
                loop {
                    let mut error_stream = String::new();
                    stderr_reader
                        .read_to_string(&mut error_stream)
                        .await
                        .unwrap();
                    if !error_stream.is_empty() {
                        assert!(error_stream.contains("NotFound")); //Todo: fix this when unwraps are removed in pod_api.rs
                        break;
                    }
                }
            }),
        )
        .await
        .unwrap()
        .unwrap();
    }

    // docker runtime test

    #[ignore]
    #[tokio::test]
    async fn test_docker_runtime() {
        _test_complete_node_api().await;
    }
}
