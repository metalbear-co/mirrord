pub mod utils;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use nix::{
        sys::signal::{self, Signal},
        unistd::Pid,
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, BufReader},
        process::Command,
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

    #[tokio::test]
    pub async fn test_file_ops() {
        let path = env!("CARGO_BIN_FILE_MIRRORD");
        let command = vec!["python3", "python-e2e/ops.py"];
        let client = setup_kube_client().await;
        let pod_namespace = "default";
        let mut env = HashMap::new();
        env.insert("MIRRORD_AGENT_IMAGE", "test");
        env.insert("MIRRORD_CHECK_VERSION", "false");
        let pod_name = get_http_echo_pod_name(&client, pod_namespace)
            .await
            .unwrap();
        let args: Vec<&str> = vec![
            "exec",
            "--pod-name",
            &pod_name,
            "-c",
            "--agent-ttl",
            "1000",
            "--enable-fs",
            "--",
        ]
        .into_iter()
        .chain(command.into_iter())
        .collect();
        let test = Command::new(path)
            .args(args)
            .envs(&env)
            .status()
            .await
            .unwrap();
        assert!(test.success());
    }

    #[tokio::test]
    pub async fn test_remote_env_vars_does_nothing_when_not_specified() {
        let mirrord_bin = env!("CARGO_BIN_FILE_MIRRORD");
        let node_command = vec![
            "node",
            "node-e2e/remote_env/test_remote_env_vars_does_nothing_when_not_specified.mjs",
        ];

        let client = setup_kube_client().await;

        let pod_namespace = "default";
        let mut env = HashMap::new();
        env.insert("MIRRORD_AGENT_IMAGE", "test");
        env.insert("MIRRORD_CHECK_VERSION", "false");

        let pod_name = get_http_echo_pod_name(&client, pod_namespace)
            .await
            .unwrap();

        let args: Vec<&str> = vec![
            "exec",
            "--pod-name",
            &pod_name,
            "-c",
            "--agent-ttl",
            "1000",
            "--",
        ]
        .into_iter()
        .chain(node_command.into_iter())
        .collect();

        let test_process = Command::new(mirrord_bin)
            .args(args)
            .envs(&env)
            .status()
            .await
            .unwrap();

        assert!(test_process.success());
    }

    /// Weird one to test, as we `panic` inside a separate thread, so the main sample app will just
    /// complete as normal.
    #[tokio::test]
    pub async fn test_remote_env_vars_panics_when_both_filters_are_specified() {
        let mirrord_bin = env!("CARGO_BIN_FILE_MIRRORD");
        let node_command = vec![
            "node",
            "node-e2e/remote_env/test_remote_env_vars_panics_when_both_filters_are_specified.mjs",
        ];

        let client = setup_kube_client().await;

        let pod_namespace = "default";
        let mut env = HashMap::new();
        env.insert("MIRRORD_AGENT_IMAGE", "test");
        env.insert("MIRRORD_CHECK_VERSION", "false");

        let pod_name = get_http_echo_pod_name(&client, pod_namespace)
            .await
            .unwrap();

        let args: Vec<&str> = vec![
            "exec",
            "--pod-name",
            &pod_name,
            "-c",
            "--agent-ttl",
            "1000",
            "-x",
            "MIRRORD_FAKE_VAR_FIRST",
            "-s",
            "MIRRORD_FAKE_VAR_SECOND",
            "--",
        ]
        .into_iter()
        .chain(node_command.into_iter())
        .collect();

        let test_process = Command::new(mirrord_bin)
            .args(args)
            .envs(&env)
            .status()
            .await
            .unwrap();

        assert!(test_process.success());
    }

    #[tokio::test]
    pub async fn test_remote_env_vars_exclude_works() {
        let mirrord_bin = env!("CARGO_BIN_FILE_MIRRORD");
        let node_command = vec![
            "node",
            "node-e2e/remote_env/test_remote_env_vars_exclude_works.mjs",
        ];

        let client = setup_kube_client().await;

        let pod_namespace = "default";
        let mut env = HashMap::new();
        env.insert("MIRRORD_AGENT_IMAGE", "test");
        env.insert("MIRRORD_CHECK_VERSION", "false");

        let pod_name = get_http_echo_pod_name(&client, pod_namespace)
            .await
            .unwrap();

        let args: Vec<&str> = vec![
            "exec",
            "--pod-name",
            &pod_name,
            "-c",
            "--agent-ttl",
            "1000",
            "-x",
            "MIRRORD_FAKE_VAR_FIRST",
            "--",
        ]
        .into_iter()
        .chain(node_command.into_iter())
        .collect();

        let test_process = Command::new(mirrord_bin)
            .args(args)
            .envs(&env)
            .status()
            .await
            .unwrap();

        assert!(test_process.success());
    }

    #[tokio::test]
    pub async fn test_remote_env_vars_include_works() {
        let mirrord_bin = env!("CARGO_BIN_FILE_MIRRORD");
        let node_command = vec![
            "node",
            "node-e2e/remote_env/test_remote_env_vars_include_works.mjs",
        ];

        let client = setup_kube_client().await;

        let pod_namespace = "default";
        let mut env = HashMap::new();
        env.insert("MIRRORD_AGENT_IMAGE", "test");
        env.insert("MIRRORD_CHECK_VERSION", "false");

        let pod_name = get_http_echo_pod_name(&client, pod_namespace)
            .await
            .unwrap();

        let args: Vec<&str> = vec![
            "exec",
            "--pod-name",
            &pod_name,
            "--agent-ttl",
            "1000",
            "-c",
            "-s",
            "MIRRORD_FAKE_VAR_FIRST",
            "--",
        ]
        .into_iter()
        .chain(node_command.into_iter())
        .collect();

        let test_process = Command::new(mirrord_bin)
            .args(args)
            .envs(&env)
            .status()
            .await
            .unwrap();

        assert!(test_process.success());
    }
}
