#[cfg(test)]

mod tests {
    use std::{fmt::Debug, net::Ipv4Addr};

    use futures_util::stream::{StreamExt, TryStreamExt};
    use k8s_openapi::{
        api::{
            apps::v1::Deployment,
            core::v1::{Namespace, Pod, Service},
        },
        apimachinery::pkg::apis::meta::v1::ObjectMeta,
        Resource,
    };
    use kube::{
        api::{DeleteParams, ListParams, PostParams},
        core::WatchEvent,
        Api, Client, Config,
    };
    use rand::{distributions::Alphanumeric, Rng};
    use rstest::*;
    use serde::{de::DeserializeOwned, Deserialize, Serialize};
    use serde_json::json;
    // 0.8
    use tokio_util::sync::{CancellationToken, DropGuard};

    pub async fn watch_resource_exists<K: Debug + Clone + DeserializeOwned>(
        api: &Api<K>,
        name: &str,
    ) {
        let params = ListParams::default()
            .fields(&format!("metadata.name={}", name))
            .timeout(10);
        let mut stream = api.watch(&params, "0").await.unwrap().boxed();
        while let Some(status) = stream.try_next().await.unwrap() {
            match status {
                WatchEvent::Modified(_) => break,
                WatchEvent::Error(s) => {
                    panic!("Error watching namespaces: {:?}", s);
                }
                _ => {}
            }
        }
    }

    fn random_string() -> String {
        let mut rand_str: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(7)
            .map(char::from)
            .collect();
        rand_str.make_ascii_lowercase();
        rand_str
    }

    #[fixture]
    pub async fn kube_client() -> Client {
        let mut config = Config::infer().await.unwrap();
        config.accept_invalid_certs = true;
        Client::try_from(config).unwrap()
    }

    struct ResourceGuard {
        guard: Option<DropGuard>,
        barrier: std::sync::Arc<std::sync::Barrier>,
    }

    impl ResourceGuard {
        /// Creates a resource and spawns a task to delete it when dropped
        /// I'm not sure why I have to add the `static here but this works?
        pub async fn create<K: Debug + Clone + DeserializeOwned + Serialize + 'static>(
            api: &Api<K>,
            name: String,
            data: &K,
        ) -> ResourceGuard {
            api.create(&PostParams::default(), &data).await.unwrap();
            let cancel_token = CancellationToken::new();
            let pod_token = cancel_token.clone();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let guard = Self {
                guard: Some(pod_token.drop_guard()),
                barrier: barrier.clone(),
            };
            let name = name.clone();
            let cloned_api = api.clone();
            tokio::spawn(async move {
                cancel_token.cancelled().await;
                cloned_api
                    .delete(&name, &DeleteParams::default())
                    .await
                    .unwrap();
                barrier.wait();
            });
            guard
        }
    }

    impl Drop for ResourceGuard {
        fn drop(&mut self) {
            let guard = self.guard.take();
            drop(guard);
            self.barrier.wait();
        }
    }

    struct EchoService {
        name: String,
        namespace: String,
        _pod: ResourceGuard,
        _service: ResourceGuard,
    }

    #[fixture]
    async fn service(
        #[future] kube_client: Client,
        #[default("default")] namespace: &str,
    ) -> EchoService {
        let kube_client = kube_client.await;
        let deployment_api: Api<Deployment> = Api::namespaced(kube_client.clone(), namespace);
        let name = random_string();
        let name = format!("http-echo-{}", name);
        let deployment: Deployment = serde_json::from_value(json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {
                "name": name,
                "labels": {
                    "app": name
                }
            },
            "spec": {
                "replicas": 1,
                "selector": {
                    "matchLabels": {
                        "app": name
                    }
                },
                "template": {
                    "metadata": {
                        "labels": {
                            "app": name
                        }
                    },
                    "spec": {
                        "containers": [
                            {
                                "name": "http-echo",
                                "image": "ealen/echo-server",
                                "ports": [
                                    {
                                        "containerPort": 80
                                    }
                                ]
                            }
                        ]
                    }
                }
            }
        }))
        .unwrap();
        let pod_guard = ResourceGuard::create(&deployment_api, name.to_string(), &deployment).await;
        watch_resource_exists(&deployment_api, &name).await;

        let service_api: Api<Service> = Api::namespaced(kube_client.clone(), namespace);
        let service: Service = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": {
                "name": &name,
                "labels": {
                    "app": &name
                }
            },
            "spec": {
                "type": "NodePort",
                "selector": {
                    "app": &name
                },
                "sessionAffinity": "None",
                "ports": [
                    {
                        "protocol": "TCP",
                        "port": 80,
                        "targetPort": 80
                    }
                ]
            }
        }))
        .unwrap();

        let service_guard = ResourceGuard::create(&service_api, name.to_string(), &service).await;
        watch_resource_exists(&service_api, "default").await;

        EchoService {
            name: name.to_string(),
            namespace: namespace.to_string(),
            _pod: pod_guard,
            _service: service_guard,
        }
    }

    #[cfg(target_os="macos")]
    fn resolve_node_host() -> String {
        #[derive(serde::Deserialize)]
        struct NodeHost {
            address: String,
        }
        let output = std::process::Command::new("colima")
            .arg("list")
            .arg("-j")
            .output()
            .unwrap()
            .stdout;
        let json: NodeHost = serde_json::from_slice(&output).unwrap();
        json.address
    }

    #[cfg(target_os="linux")]
    fn resolve_node_host() -> String {
        let output = std::process::Command::new("minikube")
            .arg("ip")
            .output()
            .unwrap()
            .stdout;
        json.address.collect()
    }

    // pub async fn get_service_url(client: &Client, namespace: &str) -> Option<String> {
    //     let service_api: Api<Service> = Api::namespaced(client.clone(), namespace);
    //     let services = service_api
    //         .list(&ListParams::default().labels("app=http-echo"))
    //         .await
    //         .unwrap();
    //     let pod_api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    //     let pods = pod_api
    //         .list(&ListParams::default().labels("app=http-echo"))
    //         .await
    //         .unwrap();
    //     let host_ip = pods
    //         .into_iter()
    //         .next()
    //         .and_then(|pod| pod.status)
    //         .and_then(|status| status.host_ip);
    //     let port = services
    //         .into_iter()
    //         .next()
    //         .and_then(|service| service.spec)
    //         .and_then(|spec| spec.ports)
    //         .and_then(|mut ports| ports.pop());
    //     Some(format!(
    //         "http://{}:{}",
    //         host_ip.unwrap(),
    //         port.unwrap().node_port.unwrap()
    //     ))
    // }
    async fn get_service_url(kube_client: Client, service: &EchoService) -> String {

        
        let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), &service.namespace);
        let pods = pod_api
            .list(&ListParams::default().labels(&format!("app={}", service.name)))
            .await
            .unwrap();
        let mut host_ip = pods.into_iter()
            .next()
            .and_then(|pod| pod.status)
            .and_then(|status| status.host_ip)
            .unwrap();
        if host_ip.parse::<Ipv4Addr>().unwrap().is_private() {
            host_ip = resolve_node_host();
        }
        let services_api: Api<Service> = Api::namespaced(kube_client.clone(), &service.namespace);
        let services = services_api
            .list(&ListParams::default().labels(&format!("app={}", service.name)))
            .await
            .unwrap();
        let port = services
            .into_iter()
            .next()
            .and_then(|service| service.spec)
            .and_then(|spec| spec.ports)
            .and_then(|mut ports| ports.pop())
            .unwrap();
        format!("http://{}:{}", host_ip, port.node_port.unwrap())
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_sanity_flow(#[future] service: EchoService, #[future] kube_client: Client) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        println!("{}", url);
        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
        assert_eq!(1, 1);
    }
}
// pub mod utils;

// #[cfg(test)]
// mod tests {
//     use std::collections::HashMap;

//     use nix::{
//         sys::signal::{self, Signal},
//         unistd::Pid,
//     };
//     use tokio::{
//         io::{AsyncBufReadExt, AsyncReadExt, BufReader},
//         process::Command,
//         time::{sleep, timeout, Duration},
//     };

//     use crate::utils::*;

//     #[tokio::test]
//     async fn test_complete_node_api() {
//         _test_complete_api("node").await;
//     }

//     #[tokio::test]
//     async fn test_complete_python_api() {
//         _test_complete_api("python").await;
//     }

//     /// Starts the Node(express.js)/Python(flask) api server, sends four different requests,
//     /// validates data, stops the server and validates if the agent job and pod are deleted.
//     async fn _test_complete_api(server: &str) {
//         let client = setup_kube_client().await;

//         let pod_namespace = "default";
//         let env: HashMap<&str, &str> = HashMap::new(); // for adding more environment variables
//         let mut server = test_server_init(&client, pod_namespace, env, server).await;

//         let service_url = get_service_url(&client, pod_namespace).await.unwrap();

//         let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
//         let mut stdout_reader = BufReader::new(server.stdout.take().unwrap());

//         // Note: to run a task in the background, don't await on it.
//         // spawn returns a JoinHandle
//         tokio::spawn(async move {
//             loop {
//                 let mut error_stream = String::new();
//                 // the following is a blocking call, so we need to spawn a task to read the
// stderr                 stderr_reader.read_line(&mut error_stream).await.unwrap();
//                 // Todo(investigate): it looks like the task reaches the panic when not kept in a
//                 // loop i.e. the buffer reads an empty string:  `thread 'main'
//                 // panicked at 'Error: '`
//                 if !error_stream.is_empty() {
//                     panic!("Error: {}", error_stream);
//                 }
//             }
//         });

//         let mut is_running = String::new();
//         let start_timeout = Duration::from_secs(10);

//         timeout(start_timeout, async {
//             loop {
//                 stdout_reader.read_line(&mut is_running).await.unwrap();
//                 if is_running == "Server listening on port 80\n" {
//                     break;
//                 }
//             }
//         })
//         .await
//         .unwrap();

//         // agent takes a bit of time to set filter and start sending traffic, this should solve
// many         // race stuff until we watch the agent logs and start sending requests after we see
//         // it had set the new filter.
//         sleep(Duration::from_millis(100)).await;
//         send_requests(service_url.as_str()).await;

//         timeout(Duration::from_secs(5), async {
//             server.wait().await.unwrap()
//         })
//         .await
//         .unwrap();
//         validate_requests(&mut stdout_reader).await;
//     }

//     #[tokio::test]
//     /// Sends a request to a different pod in the cluster (different namespace) and asserts
//     /// that no operation is performed as specified in the request by the server
//     /// as the agent pod is impersonating the pod running in the default namespace
//     async fn test_different_pod_in_cluster() {
//         let client = setup_kube_client().await;

//         let test_namespace = "test-namespace";
//         let pod_namespace = "default";
//         let env: HashMap<&str, &str> = HashMap::new();
//         let mut server = test_server_init(&client, pod_namespace, env, "node").await;

//         create_namespace(&client, test_namespace).await;
//         create_http_echo_pod(&client, test_namespace).await;

//         let service_url = get_service_url(&client, test_namespace).await.unwrap();

//         let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
//         let mut stdout_reader = BufReader::new(server.stdout.take().unwrap());

//         tokio::spawn(async move {
//             loop {
//                 let mut error_stream = String::new();
//                 stderr_reader.read_line(&mut error_stream).await.unwrap();
//                 if !error_stream.is_empty() {
//                     panic!("Error: {}", error_stream);
//                 }
//             }
//         });

//         let mut is_running = String::new();
//         let start_timeout = Duration::from_secs(10);

//         timeout(start_timeout, async {
//             stdout_reader.read_line(&mut is_running).await.unwrap();
//             assert_eq!(is_running, "Server listening on port 80\n");
//         })
//         .await
//         .unwrap();

//         // agent takes a bit of time to set filter and start sending traffic, this should solve
// many         // race stuff until we watch the agent logs and start sending requests after we see
//         // it had set the new filter.
//         sleep(Duration::from_millis(100)).await;
//         send_requests(service_url.as_str()).await;

//         // Note: Sending a SIGTERM adds an EOF to the stdout stream, so we can read it without
//         // blocking.
//         signal::kill(
//             Pid::from_raw(server.id().unwrap().try_into().unwrap()),
//             Signal::SIGTERM,
//         )
//         .unwrap();

//         server.wait().await.unwrap();

//         validate_no_requests(&mut stdout_reader).await;
//         delete_namespace(&client, test_namespace).await;
//     }

//     // agent namespace tests
//     #[tokio::test]
//     /// Creates a new k8s namespace, starts the API server with env:
//     /// MIRRORD_AGENT_NAMESPACE=namespace, asserts that the agent job and pod are created
//     /// validate data through requests to the API server
//     async fn test_good_agent_namespace() {
//         let client = setup_kube_client().await;

//         let agent_namespace = "test-namespace-agent-good";
//         let pod_namespace = "default";
//         let env = HashMap::from([("MIRRORD_AGENT_NAMESPACE", agent_namespace)]);
//         let mut server = test_server_init(&client, pod_namespace, env, "node").await;

//         create_namespace(&client, agent_namespace).await;

//         let service_url = get_service_url(&client, "default").await.unwrap();

//         let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
//         let mut stdout_reader = BufReader::new(server.stdout.take().unwrap());

//         tokio::spawn(async move {
//             loop {
//                 let mut error_stream = String::new();
//                 stderr_reader.read_line(&mut error_stream).await.unwrap();
//                 if !error_stream.is_empty() {
//                     panic!("Error: {}", error_stream);
//                 }
//             }
//         });

//         let mut is_running = String::new();
//         let start_timeout = Duration::from_secs(10);

//         timeout(start_timeout, async {
//             loop {
//                 stdout_reader.read_line(&mut is_running).await.unwrap();
//                 if is_running == "Server listening on port 80\n" {
//                     break;
//                 }
//             }
//         })
//         .await
//         .unwrap();

//         // agent takes a bit of time to set filter and start sending traffic, this should solve
// many         // race stuff until we watch the agent logs and start sending requests after we see
//         // it had set the new filter.
//         sleep(Duration::from_millis(100)).await;
//         send_requests(service_url.as_str()).await;

//         timeout(Duration::from_secs(5), async {
//             server.wait().await.unwrap()
//         })
//         .await
//         .unwrap();
//         validate_requests(&mut stdout_reader).await;

//         delete_namespace(&client, agent_namespace).await;
//     }

//     #[tokio::test]
//     /// Starts the API server with env: MIRRORD_AGENT_NAMESPACE=namespace (nonexistent),
//     /// asserts the process crashes: "NotFound" as the namespace does not exist
//     async fn test_nonexistent_agent_namespace() {
//         let client = setup_kube_client().await;
//         let agent_namespace = "nonexistent-namespace";
//         let pod_namespace = "default";
//         let env = HashMap::from([("MIRRORD_AGENT_NAMESPACE", agent_namespace)]);
//         let mut server = test_server_init(&client, pod_namespace, env, "node").await;

//         let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());

//         let timeout_duration = Duration::from_secs(5);
//         timeout(
//             timeout_duration,
//             tokio::spawn(async move {
//                 loop {
//                     let mut error_stream = String::new();
//                     stderr_reader
//                         .read_to_string(&mut error_stream)
//                         .await
//                         .unwrap();
//                     if !error_stream.is_empty() {
//                         assert!(error_stream.contains("NotFound")); //Todo: fix this when unwraps
// are removed in pod_api.rs                         break;
//                     }
//                 }
//             }),
//         )
//         .await
//         .unwrap()
//         .unwrap();
//     }

//     // pod namespace tests
//     #[tokio::test]
//     /// Creates a new k8s namespace, starts the API server with env:
//     /// MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE=namespace, validates data sent through
//     /// requests
//     async fn test_good_pod_namespace() {
//         let client = setup_kube_client().await;

//         let pod_namespace = "test-pod-namespace";
//         create_namespace(&client, pod_namespace).await;
//         create_http_echo_pod(&client, pod_namespace).await;

//         let env = HashMap::from([("MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE", pod_namespace)]);
//         let mut server = test_server_init(&client, pod_namespace, env, "node").await;

//         let service_url = get_service_url(&client, pod_namespace).await.unwrap();

//         let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
//         let mut stdout_reader = BufReader::new(server.stdout.take().unwrap());

//         tokio::spawn(async move {
//             loop {
//                 let mut error_stream = String::new();
//                 stderr_reader.read_line(&mut error_stream).await.unwrap();
//                 if !error_stream.is_empty() {
//                     panic!("Error: {}", error_stream);
//                 }
//             }
//         });

//         let mut is_running = String::new();
//         let start_timeout = Duration::from_secs(10);

//         timeout(start_timeout, async {
//             loop {
//                 stdout_reader.read_line(&mut is_running).await.unwrap();
//                 if is_running == "Server listening on port 80\n" {
//                     break;
//                 }
//             }
//         })
//         .await
//         .unwrap();

//         // agent takes a bit of time to set filter and start sending traffic, this should solve
// many         // race stuff until we watch the agent logs and start sending requests after we see
//         // it had set the new filter.
//         sleep(Duration::from_millis(100)).await;
//         send_requests(service_url.as_str()).await;

//         timeout(Duration::from_secs(5), async {
//             server.wait().await.unwrap()
//         })
//         .await
//         .unwrap();
//         validate_requests(&mut stdout_reader).await;

//         delete_namespace(&client, pod_namespace).await;
//     }

//     #[tokio::test]
//     /// Starts the API server with env: MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE=namespace
//     /// (nonexistent), asserts the process crashes: "NotFound" as the namespace does not
//     /// exist
//     async fn test_bad_pod_namespace() {
//         let client = setup_kube_client().await;

//         let pod_namespace = "default";
//         let env = HashMap::from([(
//             "MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE",
//             "nonexistent-namespace",
//         )]);
//         let mut server = test_server_init(&client, pod_namespace, env, "node").await;

//         let mut stderr_reader = BufReader::new(server.stderr.take().unwrap());
//         let timeout_duration = Duration::from_secs(5);
//         timeout(
//             timeout_duration,
//             tokio::spawn(async move {
//                 loop {
//                     let mut error_stream = String::new();
//                     stderr_reader
//                         .read_to_string(&mut error_stream)
//                         .await
//                         .unwrap();
//                     if !error_stream.is_empty() {
//                         assert!(
//                             error_stream.contains("NotFound"),
//                             "stream data: {error_stream:?}"
//                         ); //Todo: fix this when unwraps are removed in pod_api.rs
//                         break;
//                     }
//                 }
//             }),
//         )
//         .await
//         .unwrap()
//         .unwrap();
//     }

//     #[tokio::test]
//     pub async fn test_file_ops() {
//         let path = env!("CARGO_BIN_FILE_MIRRORD");
//         let command = vec!["python3", "python-e2e/ops.py"];
//         let client = setup_kube_client().await;
//         let pod_namespace = "default";
//         let mut env = HashMap::new();
//         env.insert("MIRRORD_AGENT_IMAGE", "test");
//         env.insert("MIRRORD_CHECK_VERSION", "false");
//         let pod_name = get_http_echo_pod_name(&client, pod_namespace)
//             .await
//             .unwrap();
//         let args: Vec<&str> = vec![
//             "exec",
//             "--pod-name",
//             &pod_name,
//             "-c",
//             "--agent-ttl",
//             "1000",
//             "--enable-fs",
//             "--",
//         ]
//         .into_iter()
//         .chain(command.into_iter())
//         .collect();
//         let test = Command::new(path)
//             .args(args)
//             .envs(&env)
//             .status()
//             .await
//             .unwrap();
//         assert!(test.success());
//     }

//     #[tokio::test]
//     pub async fn test_remote_env_vars_does_nothing_when_not_specified() {
//         let mirrord_bin = env!("CARGO_BIN_FILE_MIRRORD");
//         let node_command = vec![
//             "node",
//             "node-e2e/remote_env/test_remote_env_vars_does_nothing_when_not_specified.mjs",
//         ];

//         let client = setup_kube_client().await;

//         let pod_namespace = "default";
//         let mut env = HashMap::new();
//         env.insert("MIRRORD_AGENT_IMAGE", "test");
//         env.insert("MIRRORD_CHECK_VERSION", "false");

//         let pod_name = get_http_echo_pod_name(&client, pod_namespace)
//             .await
//             .unwrap();

//         let args: Vec<&str> = vec![
//             "exec",
//             "--pod-name",
//             &pod_name,
//             "-c",
//             "--agent-ttl",
//             "1000",
//             "--",
//         ]
//         .into_iter()
//         .chain(node_command.into_iter())
//         .collect();

//         let test_process = Command::new(mirrord_bin)
//             .args(args)
//             .envs(&env)
//             .status()
//             .await
//             .unwrap();

//         assert!(test_process.success());
//     }

//     /// Weird one to test, as we `panic` inside a separate thread, so the main sample app will
// just     /// complete as normal.
//     #[tokio::test]
//     pub async fn test_remote_env_vars_panics_when_both_filters_are_specified() {
//         let mirrord_bin = env!("CARGO_BIN_FILE_MIRRORD");
//         let node_command = vec![
//             "node",
//
// "node-e2e/remote_env/test_remote_env_vars_panics_when_both_filters_are_specified.mjs",         ];

//         let client = setup_kube_client().await;

//         let pod_namespace = "default";
//         let mut env = HashMap::new();
//         env.insert("MIRRORD_AGENT_IMAGE", "test");
//         env.insert("MIRRORD_CHECK_VERSION", "false");

//         let pod_name = get_http_echo_pod_name(&client, pod_namespace)
//             .await
//             .unwrap();

//         let args: Vec<&str> = vec![
//             "exec",
//             "--pod-name",
//             &pod_name,
//             "-c",
//             "--agent-ttl",
//             "1000",
//             "-x",
//             "MIRRORD_FAKE_VAR_FIRST",
//             "-s",
//             "MIRRORD_FAKE_VAR_SECOND",
//             "--",
//         ]
//         .into_iter()
//         .chain(node_command.into_iter())
//         .collect();

//         let test_process = Command::new(mirrord_bin)
//             .args(args)
//             .envs(&env)
//             .status()
//             .await
//             .unwrap();

//         assert!(test_process.success());
//     }

//     #[tokio::test]
//     pub async fn test_remote_env_vars_exclude_works() {
//         let mirrord_bin = env!("CARGO_BIN_FILE_MIRRORD");
//         let node_command = vec![
//             "node",
//             "node-e2e/remote_env/test_remote_env_vars_exclude_works.mjs",
//         ];

//         let client = setup_kube_client().await;

//         let pod_namespace = "default";
//         let mut env = HashMap::new();
//         env.insert("MIRRORD_AGENT_IMAGE", "test");
//         env.insert("MIRRORD_CHECK_VERSION", "false");

//         let pod_name = get_http_echo_pod_name(&client, pod_namespace)
//             .await
//             .unwrap();

//         let args: Vec<&str> = vec![
//             "exec",
//             "--pod-name",
//             &pod_name,
//             "-c",
//             "--agent-ttl",
//             "1000",
//             "-x",
//             "MIRRORD_FAKE_VAR_FIRST",
//             "--",
//         ]
//         .into_iter()
//         .chain(node_command.into_iter())
//         .collect();

//         let test_process = Command::new(mirrord_bin)
//             .args(args)
//             .envs(&env)
//             .status()
//             .await
//             .unwrap();

//         assert!(test_process.success());
//     }

//     #[tokio::test]
//     pub async fn test_remote_env_vars_include_works() {
//         let mirrord_bin = env!("CARGO_BIN_FILE_MIRRORD");
//         let node_command = vec![
//             "node",
//             "node-e2e/remote_env/test_remote_env_vars_include_works.mjs",
//         ];

//         let client = setup_kube_client().await;

//         let pod_namespace = "default";
//         let mut env = HashMap::new();
//         env.insert("MIRRORD_AGENT_IMAGE", "test");
//         env.insert("MIRRORD_CHECK_VERSION", "false");

//         let pod_name = get_http_echo_pod_name(&client, pod_namespace)
//             .await
//             .unwrap();

//         let args: Vec<&str> = vec![
//             "exec",
//             "--pod-name",
//             &pod_name,
//             "--agent-ttl",
//             "1000",
//             "-c",
//             "-s",
//             "MIRRORD_FAKE_VAR_FIRST",
//             "--",
//         ]
//         .into_iter()
//         .chain(node_command.into_iter())
//         .collect();

//         let test_process = Command::new(mirrord_bin)
//             .args(args)
//             .envs(&env)
//             .status()
//             .await
//             .unwrap();

//         assert!(test_process.success());
//     }
// }
