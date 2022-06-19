pub mod utils;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use k8s_openapi::api::{batch::v1::Job, core::v1::Pod};
    use kube::{api::ListParams, Api};
    use nix::{
        sys::signal::{self, Signal},
        unistd::Pid,
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, BufReader},
        time::{sleep, timeout, Duration},
    };

    use crate::utils::*;

    #[tokio::test]
    async fn test_complete_node_api() {
        _test_complete_api("node").await;
    }

    #[tokio::test]
    async fn test_complete_python_api() {
        _test_complete_api("python").await;
    }

    /// Starts the Node(express.js)/Python(flask) api server, sends four different requests,
    /// validates data, stops the server and validates if the agent job and pod are deleted.
    async fn _test_complete_api(server: &str) {
        let client = setup_kube_client().await;

        let pod_namespace = "default";
        let env: HashMap<&str, &str> = HashMap::new(); // for adding more environment variables
        let mut server = test_server_init(&client, pod_namespace, env, server).await;

        let service_url = get_service_url(&client, pod_namespace).await.unwrap();

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
        let mut stdout_reader = BufReader::new(server.stdout.take().unwrap());

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

        let mut is_running = String::new();
        let start_timeout = Duration::from_secs(10);

        timeout(start_timeout, async {
            loop {
                stdout_reader.read_line(&mut is_running).await.unwrap();
                if is_running == "Server listening on port 80\n" {
                    break;
                }
            }
        })
        .await
        .unwrap();

        // agent takes a bit of time to set filter and start sending traffic, this should solve many
        // race stuff until we watch the agent logs and start sending requests after we see
        // it had set the new filter.
        sleep(Duration::from_millis(100)).await;
        send_requests(service_url.as_str()).await;

        timeout(Duration::from_secs(5), async {
            server.wait().await.unwrap()
        })
        .await
        .unwrap();
        validate_requests(&mut stdout_reader).await;

        let jobs_api: Api<Job> = Api::namespaced(client.clone(), "default");
        let jobs = jobs_api.list(&ListParams::default()).await.unwrap();
        // assuming only one job is running
        // to make the tests parallel we need to figure a way to get the exact job name when len() >
        // 1
        assert!(jobs.items.len() > 0);

        let pods_api: Api<Pod> = Api::namespaced(client.clone(), "default");
        let pods = pods_api.list(&ListParams::default()).await.unwrap();
        assert!(pods.items.len() > 1);

        let cleanup_timeout = Duration::from_secs(35);
        timeout(
            cleanup_timeout,
            tokio::spawn(async move {
                // verify cleanup
                loop {
                    let updated_jobs = jobs_api.list(&ListParams::default()).await.unwrap();
                    let updated_pods = pods_api.list(&ListParams::default()).await.unwrap(); // only the http-echo pod should exist
                    if updated_pods.items.len() == 1 && updated_jobs.items.is_empty() {
                        let http_echo_pod = updated_pods.items[0].metadata.name.clone().unwrap();
                        assert!(http_echo_pod.contains("http-echo"));
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    /// Sends a request to a different pod in the cluster (different namespace) and asserts
    /// that no operation is performed as specified in the request by the server
    /// as the agent pod is impersonating the pod running in the default namespace
    async fn test_different_pod_in_cluster() {
        let client = setup_kube_client().await;

        let test_namespace = "test-namespace";
        let pod_namespace = "default";
        let env: HashMap<&str, &str> = HashMap::new();
        let mut server = test_server_init(&client, pod_namespace, env, "node").await;

        create_namespace(&client, test_namespace).await;
        create_http_echo_pod(&client, test_namespace).await;

        let service_url = get_service_url(&client, test_namespace).await.unwrap();

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
        let mut stdout_reader = BufReader::new(server.stdout.take().unwrap());

        tokio::spawn(async move {
            loop {
                let mut error_stream = String::new();
                stderr_reader.read_line(&mut error_stream).await.unwrap();
                if !error_stream.is_empty() {
                    panic!("Error: {}", error_stream);
                }
            }
        });

        let mut is_running = String::new();
        let start_timeout = Duration::from_secs(10);

        timeout(start_timeout, async {
            stdout_reader.read_line(&mut is_running).await.unwrap();
            assert_eq!(is_running, "Server listening on port 80\n");
        })
        .await
        .unwrap();

        // agent takes a bit of time to set filter and start sending traffic, this should solve many
        // race stuff until we watch the agent logs and start sending requests after we see
        // it had set the new filter.
        sleep(Duration::from_millis(100)).await;
        send_requests(service_url.as_str()).await;

        // Note: Sending a SIGTERM adds an EOF to the stdout stream, so we can read it without
        // blocking.
        signal::kill(
            Pid::from_raw(server.id().unwrap().try_into().unwrap()),
            Signal::SIGTERM,
        )
        .unwrap();

        server.wait().await.unwrap();

        validate_no_requests(&mut stdout_reader).await;
        delete_namespace(&client, test_namespace).await;
    }

    // agent namespace tests
    #[tokio::test]
    /// Creates a new k8s namespace, starts the API server with env:
    /// MIRRORD_AGENT_NAMESPACE=namespace, asserts that the agent job and pod are created
    /// validate data through requests to the API server
    async fn test_good_agent_namespace() {
        let client = setup_kube_client().await;

        let agent_namespace = "test-namespace-agent-good";
        let pod_namespace = "default";
        let env = HashMap::from([("MIRRORD_AGENT_NAMESPACE", agent_namespace)]);
        let mut server = test_server_init(&client, pod_namespace, env, "node").await;

        create_namespace(&client, agent_namespace).await;

        let service_url = get_service_url(&client, "default").await.unwrap();

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
        let mut stdout_reader = BufReader::new(server.stdout.take().unwrap());

        tokio::spawn(async move {
            loop {
                let mut error_stream = String::new();
                stderr_reader.read_line(&mut error_stream).await.unwrap();
                if !error_stream.is_empty() {
                    panic!("Error: {}", error_stream);
                }
            }
        });

        let mut is_running = String::new();
        let start_timeout = Duration::from_secs(10);

        timeout(start_timeout, async {
            loop {
                stdout_reader.read_line(&mut is_running).await.unwrap();
                if is_running == "Server listening on port 80\n" {
                    break;
                }
            }
        })
        .await
        .unwrap();

        // agent takes a bit of time to set filter and start sending traffic, this should solve many
        // race stuff until we watch the agent logs and start sending requests after we see
        // it had set the new filter.
        sleep(Duration::from_millis(100)).await;
        send_requests(service_url.as_str()).await;

        timeout(Duration::from_secs(5), async {
            server.wait().await.unwrap()
        })
        .await
        .unwrap();
        validate_requests(&mut stdout_reader).await;

        let jobs_api: Api<Job> = Api::namespaced(client.clone(), agent_namespace);
        let jobs = jobs_api.list(&ListParams::default()).await.unwrap();
        assert_eq!(jobs.items.len(), 1);

        let pods_api: Api<Pod> = Api::namespaced(client.clone(), agent_namespace);
        let pods = pods_api.list(&ListParams::default()).await.unwrap();
        assert_eq!(pods.items.len(), 1);
        delete_namespace(&client, agent_namespace).await;
    }

    #[tokio::test]
    /// Starts the API server with env: MIRRORD_AGENT_NAMESPACE=namespace (nonexistent),
    /// asserts the process crashes: "NotFound" as the namespace does not exist
    async fn test_nonexistent_agent_namespace() {
        let client = setup_kube_client().await;
        let agent_namespace = "nonexistent-namespace";
        let pod_namespace = "default";
        let env = HashMap::from([("MIRRORD_AGENT_NAMESPACE", agent_namespace)]);
        let mut server = test_server_init(&client, pod_namespace, env, "node").await;

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
    /// Creates a new k8s namespace, starts the API server with env:
    /// MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE=namespace, validates data sent through
    /// requests
    async fn test_good_pod_namespace() {
        let client = setup_kube_client().await;

        let pod_namespace = "test-pod-namespace";
        create_namespace(&client, pod_namespace).await;
        create_http_echo_pod(&client, pod_namespace).await;

        let env = HashMap::from([("MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE", pod_namespace)]);
        let mut server = test_server_init(&client, pod_namespace, env, "node").await;

        let service_url = get_service_url(&client, pod_namespace).await.unwrap();

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
        let mut stdout_reader = BufReader::new(server.stdout.take().unwrap());

        tokio::spawn(async move {
            loop {
                let mut error_stream = String::new();
                stderr_reader.read_line(&mut error_stream).await.unwrap();
                if !error_stream.is_empty() {
                    panic!("Error: {}", error_stream);
                }
            }
        });

        let mut is_running = String::new();
        let start_timeout = Duration::from_secs(10);

        timeout(start_timeout, async {
            loop {
                stdout_reader.read_line(&mut is_running).await.unwrap();
                if is_running == "Server listening on port 80\n" {
                    break;
                }
            }
        })
        .await
        .unwrap();

        // agent takes a bit of time to set filter and start sending traffic, this should solve many
        // race stuff until we watch the agent logs and start sending requests after we see
        // it had set the new filter.
        sleep(Duration::from_millis(100)).await;
        send_requests(service_url.as_str()).await;

        timeout(Duration::from_secs(5), async {
            server.wait().await.unwrap()
        })
        .await
        .unwrap();
        validate_requests(&mut stdout_reader).await;

        delete_namespace(&client, pod_namespace).await;
    }

    #[tokio::test]
    /// Starts the API server with env: MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE=namespace
    /// (nonexistent), asserts the process crashes: "NotFound" as the namespace does not
    /// exist
    async fn test_bad_pod_namespace() {
        let client = setup_kube_client().await;

        let pod_namespace = "default";
        let env = HashMap::from([(
            "MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE",
            "nonexistent-namespace",
        )]);
        let mut server = test_server_init(&client, pod_namespace, env, "node").await;

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
                        assert!(
                            error_stream.contains("NotFound"),
                            "stream data: {error_stream:?}"
                        ); //Todo: fix this when unwraps are removed in pod_api.rs
                        break;
                    }
                }
            }),
        )
        .await
        .unwrap()
        .unwrap();
    }
}
