#[cfg(test)]

mod tests {
    use std::{collections::HashMap, fmt::Debug, net::Ipv4Addr, time::{Duration, Instant}, sync::{Arc, Mutex}, process::Stdio};

    use reqwest::{StatusCode, Method};
    use futures_util::stream::{StreamExt, TryStreamExt};
    use k8s_openapi::{
        api::{
            apps::v1::Deployment,
            core::v1::{Namespace, Pod, ReadNamespacedPodOptional, Service},
        },
        apimachinery::pkg::apis::meta::v1::ObjectMeta,
        Resource,
    };
    use kube::{
        api::{DeleteParams, ListParams, PostParams},
        core::WatchEvent,
        Api, Client, Config, runtime::wait::{conditions::is_pod_running, await_condition},
    };
    use rand::{distributions::Alphanumeric, Rng};
    use rstest::*;
    use serde::{de::DeserializeOwned, Deserialize, Serialize};
    use serde_json::json;
    use tokio::{
        process::{Child, Command, ChildStderr},
        time::timeout, io::{BufReader, AsyncBufReadExt, AsyncReadExt},
    };
    // 0.8
    use tokio_util::sync::{CancellationToken, DropGuard};


    static TEXT: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";

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

    // lazy_static! {
    //     static ref SERVERS: HashMap<&'static str, Vec<&'static str>> = HashMap::from([
    //         ("python", vec!["python3", "-u", "python-e2e/app.py"]),
    //         ("node", vec!["node", "node-e2e/app.js"])
    //     ]);
    // }
    

    enum Application {
        // Python,
        Node,
    }


    struct TestProcess {
        pub child: Child,
        stderr: Arc<Mutex<String>>,
        stdout: Arc<Mutex<String>>,
    }

    impl TestProcess {
        fn get_stdout(&self) -> String {
            self.stdout.lock().unwrap().clone()
        }

        fn get_stderr(&self) -> String {
            self.stderr.lock().unwrap().clone()
        }

        fn assert_stderr(&self) {
            assert!(self.stderr.lock().unwrap().is_empty());
        }

        fn wait_for_line(&self, timeout: Duration, line: &str) {
            let now = std::time::Instant::now();
            while now.elapsed() < timeout {
                let stdout = self.get_stdout();
                if stdout.contains(line) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            panic!("Timeout waiting for line: {}", line);
        } 

        fn from_child(mut child: Child) -> TestProcess {
            let stderr_data = Arc::new(Mutex::new(String::new()));
            let stdout_data = Arc::new(Mutex::new(String::new()));
            let child_stderr = child.stderr.take().unwrap();
            let child_stdout = child.stdout.take().unwrap();
            let stderr_data_reader = stderr_data.clone();
            let stdout_data_reader = stdout_data.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(child_stderr);
                let mut buf = [0; 1024];
                loop {
                    let n = reader.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    
                    let string = String::from_utf8_lossy(&buf[..n]);
                    eprintln!("stderr: {}", string);
                    {
                        stderr_data_reader.lock().unwrap().push_str(&string);
                    }
                }
            });
            tokio::spawn(async move {
                let mut reader = BufReader::new(child_stdout);
                let mut buf = [0; 1024];
                loop {
                    let n = reader.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    let string = String::from_utf8_lossy(&buf[..n]);
                    println!("stdout: {}", string);
                    {
                        stdout_data_reader.lock().unwrap().push_str(&string);
                    }
                    }
                }
            );
            TestProcess {
                child,
                stderr: stderr_data,
                stdout: stdout_data,
            }
        }

    }


    impl Application {
        async fn run(&self, pod_name: &str, namespace: Option<&str>) -> TestProcess {
            let path = env!("CARGO_BIN_FILE_MIRRORD");
            let app_args = match self {
                // Application::Python => vec!["/usr/local/bin/python3", "-u", "python-e2e/app.py"],
                Application::Node => vec!["node", "node-e2e/app.js"],
            };
            let mut mirrord_args = vec!["exec", "--pod-name", pod_name, "-c"];
            if let Some(namespace) = namespace {
                mirrord_args.extend(["--pod-namespace", namespace].into_iter());
            }
            mirrord_args.push("--");
            let args: Vec<&str> = mirrord_args
                .into_iter()
                .chain(app_args.into_iter())
                .collect();
            // used by the CI, to load the image locally:
            // docker build -t test . -f mirrord-agent/Dockerfile
            // minikube load image test:latest
            let mut env = HashMap::new();
            env.insert("MIRRORD_AGENT_IMAGE", "test");
            env.insert("MIRRORD_CHECK_VERSION", "false");
            env.insert("MIRRORD_AGENT_RUST_LOG", "debug");
            env.insert("RUST_LOG", "debug");
            let server = Command::new(path)
                .args(args)
                .envs(env)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            
            TestProcess::from_child(server)
        }
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
            let resource_token = cancel_token.clone();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let guard = Self {
                guard: Some(resource_token.drop_guard()),
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

    pub struct EchoService {
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

    #[cfg(target_os = "macos")]
    fn resolve_node_host() -> String {
        // We assume it's Docker for Mac
        "127.0.0.1".to_string()
    } 

    #[cfg(target_os = "linux")]
    fn resolve_node_host() -> String {
        let output = std::process::Command::new("minikube")
            .arg("ip")
            .output()
            .unwrap()
            .stdout;
        output.into_iter().collect()
    }

    async fn get_service_url(kube_client: Client, service: &EchoService) -> String {
        let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), &service.namespace);
        let pods = pod_api
            .list(&ListParams::default().labels(&format!("app={}", service.name)))
            .await
            .unwrap();
        let mut host_ip = pods
            .into_iter()
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

    pub async fn get_pod_instance(client: &Client, service: &EchoService) -> Option<String> {
        let pod_api: Api<Pod> = Api::namespaced(client.clone(), &service.namespace);
        let pods = pod_api
            .list(&ListParams::default().labels(&format!("app={}", service.name)))
            .await
            .unwrap();
        let pod = pods.iter().next().and_then(|pod| pod.metadata.name.clone());
        pod
    }

    pub async fn send_requests(url: &str) {
        println!("{url}");
        let client = reqwest::Client::new();
        let res = client
            .get(url)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back
        res.bytes().await.unwrap();

        let client = reqwest::Client::new();
        let res = client
            .post(url)
            .body(TEXT)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back
        res.bytes().await.unwrap();

        let client = reqwest::Client::new();
        let res = client
            .put(url)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back
        res.bytes().await.unwrap();

        let client = reqwest::Client::new();
        let res = client
            .delete(url)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back
        res.bytes().await.unwrap();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_mirror_http_traffic(
        #[future] service: EchoService,
        #[future] kube_client: Client,
        #[values(
            // Application::Python,
            Application::Node)] application: Application,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let pod_name = get_pod_instance(&kube_client, &service).await.unwrap();
        let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), &service.namespace);
        await_condition(pod_api, &pod_name, is_pod_running()).await;
        let mut process = application.run(&pod_name, Some(&service.namespace)).await;
        process.wait_for_line(Duration::from_secs(10), "Server listening on port 80");
        send_requests(&url).await;
        timeout(Duration::from_secs(40), process.child.wait()).await.unwrap().unwrap();
        process.assert_stderr();
    }
}


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
