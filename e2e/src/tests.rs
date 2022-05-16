use std::{collections::HashMap, path::Path};

use k8s_openapi::api::{batch::v1::Job, core::v1::Pod};
use kube::{api::ListParams, Api};
use reqwest::Method;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    time::{sleep, timeout, Duration},
};

use crate::utils::*;

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    // starts the node(express.js) api server, sends four different requests, validates data,
    // stops the server and validates if the agent job and pod are deleted
    async fn test_complete_node_api() {
        let client = setup_kube_client().await;

        let service_url = get_service_url(&client, "default").await.unwrap();
        let pod_name = get_nginx_pod_name(&client, "default").await.unwrap();
        let command = vec!["node", "node-e2e/app.js"];
        let env: HashMap<&str, &str> = HashMap::new(); // for adding more environment variables
        let mut server = start_node_server(&pod_name, command, env);

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
        let child_stdout = server.stdout.take().unwrap();

        setup_panic_hook();

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
        let validation_duration = Duration::from_secs(30);
        timeout(
            validation_duration,
            validate_requests(child_stdout, service_url.as_str(), &mut server),
        )
        .await
        .unwrap();

        // look for the agent job and pod
        let jobs_api: Api<Job> = Api::namespaced(client.clone(), "default");
        let jobs = jobs_api.list(&ListParams::default()).await.unwrap();
        // assuming only one job is running
        // to make the tests parallel we need to figure a way to get the exact job name when len() >
        // 1
        assert_eq!(jobs.items.len(), 1);
        let job_name = jobs.items[0].metadata.name.clone().unwrap();

        let pods_api: Api<Pod> = Api::namespaced(client.clone(), "default");
        let pods = pods_api.list(&ListParams::default()).await.unwrap();
        let pod_name = pods
            .items
            .iter()
            .filter_map(|pod| {
                if pod.metadata.name.clone().unwrap().contains(&job_name) {
                    Some(pod.metadata.name.clone().unwrap())
                } else {
                    None
                }
            })
            .next()
            .unwrap();
        assert!(pod_name.contains(&job_name));
        sleep(Duration::from_secs(35)).await;

        //verify cleanup
        let updated_jobs = jobs_api.list(&ListParams::default()).await.unwrap();
        assert_eq!(updated_jobs.items.len(), 0);

        let updated_pods = pods_api.list(&ListParams::default()).await.unwrap();
        assert_eq!(updated_pods.items.len(), 1); // only the nginx pod should exist
    }

    #[tokio::test]
    // we send a request to a different pod in the cluster (different namespace) and assert
    // that no operation is performed as specified in the request by the server
    // as the agent pod is impersonating the pod running in the default namespace
    async fn test_different_pod_in_cluster() {
        let client = setup_kube_client().await;

        let namespace = "test-namespace";
        create_namespace(&client, namespace).await;
        create_nginx_pod(&client, namespace).await;
        sleep(Duration::from_secs(2)).await; // we need a short sleep here, otherwise the test panics

        let service_url = get_service_url(&client, namespace).await.unwrap();
        let pod_name = get_nginx_pod_name(&client, "default").await.unwrap();
        let command = vec!["node", "node-e2e/app.js"];
        let env: HashMap<&str, &str> = HashMap::new();
        let mut server = start_node_server(&pod_name, command, env);

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
        setup_panic_hook();

        tokio::spawn(async move {
            loop {
                let mut error_stream = String::new();
                stderr_reader.read_line(&mut error_stream).await.unwrap();
                if !error_stream.is_empty() {
                    panic!("Error: {}", error_stream);
                }
            }
        });

        let mut reader = BufReader::new(server.stdout.take().unwrap());
        let mut line = String::new();

        let timeout_duration = Duration::from_secs(10);
        timeout(timeout_duration, reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();

        assert!(line.contains("Server listening on port 80"));
        reqwest_request(service_url.as_str(), Method::PUT).await;
        sleep(Duration::from_secs(5)).await;
        assert!(!Path::new("/tmp/test").exists()); // the API creates a file in /tmp/, which should not exist
                                                   // cleanup
        server.kill().await.unwrap();
        delete_namespace(&client, namespace).await;
    }

    // agent namespace tests
    #[tokio::test]
    async fn test_good_agent_namespace() {
        // creates a new k8s namespace, starts the API server with env:
        // MIRRORD_AGENT_NAMESPACE=namespace, asserts that the agent job and pod are created
        // validate data through requests to the API
        let client = setup_kube_client().await;

        let namespace = "test-namespace-agent-good";
        create_namespace(&client, namespace).await;
        let service_url = get_service_url(&client, "default").await.unwrap();
        let pod_name = get_nginx_pod_name(&client, "default").await.unwrap();
        let env = HashMap::from([("MIRRORD_AGENT_NAMESPACE", namespace)]);
        let command = vec!["node", "node-e2e/app.js"];
        let mut server = start_node_server(&pod_name, command, env);

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
        let child_stdout = server.stdout.take().unwrap();

        setup_panic_hook();

        tokio::spawn(async move {
            loop {
                let mut error_stream = String::new();
                stderr_reader.read_line(&mut error_stream).await.unwrap();
                if !error_stream.is_empty() {
                    panic!("Error: {}", error_stream);
                }
            }
        });

        let validation_duration = Duration::from_secs(30);
        timeout(
            validation_duration,
            validate_requests(child_stdout, service_url.as_str(), &mut server),
        )
        .await
        .unwrap();

        let jobs_api: Api<Job> = Api::namespaced(client.clone(), namespace);
        let jobs = jobs_api.list(&ListParams::default()).await.unwrap();
        assert_eq!(jobs.items.len(), 1);
        let job_name = jobs.items[0].metadata.name.clone().unwrap();

        let pods_api: Api<Pod> = Api::namespaced(client.clone(), namespace);
        let pods = pods_api.list(&ListParams::default()).await.unwrap();
        let pod_name = pods
            .items
            .iter()
            .filter_map(|pod| {
                if pod.metadata.name.clone().unwrap().contains(&job_name) {
                    Some(pod.metadata.name.clone().unwrap())
                } else {
                    None
                }
            })
            .next()
            .unwrap();
        assert!(pod_name.contains(&job_name));
        delete_namespace(&client, namespace).await;
    }

    #[tokio::test]
    async fn test_nonexistent_agent_namespace() {
        // starts the API server with env: MIRRORD_AGENT_NAMESPACE=namespace (nonexistent),
        // asserts the process crashes: "NotFound" as the namespace does not exist
        let client = setup_kube_client().await;

        let nonexistent_namespace = "nonexistent-namespace";
        let pod_name = get_nginx_pod_name(&client, "default").await.unwrap();
        let env = HashMap::from([("MIRRORD_AGENT_NAMESPACE", nonexistent_namespace)]);
        let mut server = start_node_server(&pod_name, vec!["node", "node-e2e/app.js"], env);

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());

        setup_panic_hook();

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
    async fn test_good_pod_namespace() {
        // creates a new k8s namespace, starts the API server with env:
        // MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE=namespace, validates data sent through
        // requests
        let client = setup_kube_client().await;

        let namespace = "test-pod-namespace";
        create_namespace(&client, namespace).await;
        create_nginx_pod(&client, namespace).await;

        // need to sleep here, otherwise pod_api.rs panics
        sleep(Duration::from_secs(5)).await;

        let service_url = get_service_url(&client, namespace).await.unwrap();

        let pod_name = get_nginx_pod_name(&client, namespace).await.unwrap();
        let command = vec!["node", "node-e2e/app.js"];
        let env = HashMap::from([("MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE", namespace)]);
        let mut server = start_node_server(&pod_name, command, env);

        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
        let child_stdout = server.stdout.take().unwrap();

        setup_panic_hook();

        tokio::spawn(async move {
            loop {
                let mut error_stream = String::new();
                stderr_reader.read_line(&mut error_stream).await.unwrap();
                if !error_stream.is_empty() {
                    panic!("Error: {}", error_stream);
                }
            }
        });

        let validation_duration = Duration::from_secs(30);
        timeout(
            validation_duration,
            validate_requests(child_stdout, service_url.as_str(), &mut server),
        )
        .await
        .unwrap();

        delete_namespace(&client, namespace).await;
    }

    #[tokio::test]
    async fn test_bad_pod_namespace() {
        // starts the API server with env: MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE=namespace
        // (nonexistent), asserts the process crashes: "NotFound" as the namespace does not
        // exist
        let client = setup_kube_client().await;

        let nonexistent_namespace = "nonexistent-pod-namespace";
        let pod_name = get_nginx_pod_name(&client, "default").await.unwrap();
        let env = HashMap::from([(
            "MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE",
            nonexistent_namespace,
        )]);
        let mut server = start_node_server(&pod_name, vec!["node", "node-e2e/app.js"], env);
        let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());

        setup_panic_hook();

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
}
