#[cfg(test)]

mod tests {
    use std::{
        collections::HashMap,
        fmt::Debug,
        net::Ipv4Addr,
        process::Stdio,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use futures_util::stream::{StreamExt, TryStreamExt};
    use k8s_openapi::api::{
        apps::v1::Deployment,
        core::v1::{Pod, Service},
    };
    use kube::{
        api::{DeleteParams, ListParams, PostParams},
        core::WatchEvent,
        runtime::wait::{await_condition, conditions::is_pod_running},
        Api, Client, Config,
    };
    use rand::{distributions::Alphanumeric, Rng};
    use reqwest::StatusCode;
    use rstest::*;
    use serde::{de::DeserializeOwned, Serialize};
    use serde_json::json;
    use tempdir::TempDir;
    use tokio::{
        io::{AsyncReadExt, BufReader},
        process::{Child, Command},
        time::timeout,
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

    enum Application {
        PythonHTTP,
        NodeHTTP,
    }

    struct TestProcess {
        pub child: Child,
        stderr: Arc<Mutex<String>>,
        stdout: Arc<Mutex<String>>,
        // Keeps tempdir existing while process is running.
        _tempdir: TempDir,
    }

    impl TestProcess {
        fn get_stdout(&self) -> String {
            self.stdout.lock().unwrap().clone()
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

        fn from_child(mut child: Child, tempdir: TempDir) -> TestProcess {
            let stderr_data = Arc::new(Mutex::new(String::new()));
            let stdout_data = Arc::new(Mutex::new(String::new()));
            let child_stderr = child.stderr.take().unwrap();
            let child_stdout = child.stdout.take().unwrap();
            let stderr_data_reader = stderr_data.clone();
            let stdout_data_reader = stdout_data.clone();
            let pid = child.id().unwrap();

            tokio::spawn(async move {
                let mut reader = BufReader::new(child_stderr);
                let mut buf = [0; 1024];
                loop {
                    let n = reader.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }

                    let string = String::from_utf8_lossy(&buf[..n]);
                    eprintln!("stderr {pid}: {}", string);
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
                    println!("stdout pid {pid}: {}", string);
                    {
                        stdout_data_reader.lock().unwrap().push_str(&string);
                    }
                }
            });
            TestProcess {
                child,
                stderr: stderr_data,
                stdout: stdout_data,
                _tempdir: tempdir,
            }
        }
    }

    impl Application {
        async fn run(
            &self,
            pod_name: &str,
            namespace: Option<&str>,
            args: Option<Vec<&str>>,
        ) -> TestProcess {
            let process_cmd = match self {
                Application::PythonHTTP => {
                    vec!["python3", "-u", "python-e2e/app.py"]
                }
                Application::NodeHTTP => vec!["node", "node-e2e/app.js"],
            };
            run(process_cmd, pod_name, namespace, args).await
        }
    }

    async fn run(
        process_cmd: Vec<&str>,
        pod_name: &str,
        namespace: Option<&str>,
        args: Option<Vec<&str>>,
    ) -> TestProcess {
        let path = env!("CARGO_BIN_FILE_MIRRORD");
        let temp_dir = tempdir::TempDir::new("test").unwrap();
        let mut mirrord_args = vec![
            "exec",
            "--pod-name",
            pod_name,
            "-c",
            "--extract-path",
            temp_dir.path().to_str().unwrap(),
        ];
        if let Some(namespace) = namespace {
            mirrord_args.extend(["--pod-namespace", namespace].into_iter());
        }
        if let Some(args) = args {
            mirrord_args.extend(args.into_iter());
        }
        mirrord_args.push("--");
        let args: Vec<&str> = mirrord_args
            .into_iter()
            .chain(process_cmd.into_iter())
            .collect();
        // used by the CI, to load the image locally:
        // docker build -t test . -f mirrord-agent/Dockerfile
        // minikube load image test:latest
        let mut env = HashMap::new();
        env.insert("MIRRORD_AGENT_IMAGE", "test");
        env.insert("MIRRORD_CHECK_VERSION", "false");
        env.insert("MIRRORD_AGENT_RUST_LOG", "debug");
        env.insert("MIRRORD_IMPERSONATED_CONTAINER_NAME", "test");
        env.insert("RUST_LOG", "debug");
        let server = Command::new(path)
            .args(args.clone())
            .envs(env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        println!(
            "executed mirrord with args {args:?} pid {}",
            server.id().unwrap()
        );
        // We need to hold temp dir until the process is finished
        TestProcess::from_child(server, temp_dir)
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
        pod_name: String,
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
                                "name": "test",
                                "image": "ghcr.io/metalbear-co/mirrord-pytest:latest",
                                "ports": [
                                    {
                                        "containerPort": 80
                                    }
                                ],
                                "env": [
                                    {
                                      "name": "MIRRORD_FAKE_VAR_FIRST",
                                      "value": "mirrord.is.running"
                                    },
                                    {
                                      "name": "MIRRORD_FAKE_VAR_SECOND",
                                      "value": "7777"
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

        let pod_name = get_pod_instance(&kube_client, &name, &namespace)
            .await
            .unwrap();
        let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), &namespace);
        await_condition(pod_api, &pod_name, is_pod_running())
            .await
            .unwrap();

        EchoService {
            name: name.to_string(),
            namespace: namespace.to_string(),
            pod_name,
            _pod: pod_guard,
            _service: service_guard,
        }
    }

    #[cfg(target_os = "macos")]
    fn resolve_node_host() -> String {
        if std::env::var("USE_MINIKUBE").is_ok() {
            let output = std::process::Command::new("minikube")
                .arg("ip")
                .output()
                .unwrap()
                .stdout;
            String::from_utf8_lossy(&output).to_string()
        } else {
            // We assume it's Docker for Mac
            "127.0.0.1".to_string()
        }
    }

    #[cfg(target_os = "linux")]
    fn resolve_node_host() -> String {
        let output = std::process::Command::new("minikube")
            .arg("ip")
            .output()
            .unwrap()
            .stdout;
        String::from_utf8_lossy(&output).to_string()
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

    pub async fn get_pod_instance(
        client: &Client,
        app_name: &str,
        namespace: &str,
    ) -> Option<String> {
        let pod_api: Api<Pod> = Api::namespaced(client.clone(), namespace);
        let pods = pod_api
            .list(&ListParams::default().labels(&format!("app={}", app_name)))
            .await
            .unwrap();
        let pod = pods.iter().next().and_then(|pod| pod.metadata.name.clone());
        pod
    }

    pub async fn send_requests(url: &str) {
        // Create client for each request until we have a match between local app and remote app
        // as connection state is flaky
        println!("{url}");
        let client = reqwest::Client::new();
        let res = client.get(url).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back
        res.bytes().await.unwrap();

        let client = reqwest::Client::new();
        let res = client.post(url).body(TEXT).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back
        res.bytes().await.unwrap();

        let client = reqwest::Client::new();
        let res = client.put(url).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back
        res.bytes().await.unwrap();

        let client = reqwest::Client::new();
        let res = client.delete(url).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back
        res.bytes().await.unwrap();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_mirror_http_traffic(
        #[future] service: EchoService,
        #[future] kube_client: Client,
        #[values(Application::PythonHTTP, Application::NodeHTTP)] application: Application,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let mut process = application
            .run(&service.pod_name, Some(&service.namespace), None)
            .await;
        process.wait_for_line(Duration::from_secs(30), "Server listening on port 80");
        send_requests(&url).await;
        timeout(Duration::from_secs(40), process.child.wait())
            .await
            .unwrap()
            .unwrap();
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore] // TODO: Fix this test
    pub async fn test_file_ops(#[future] service: EchoService) {
        let service = service.await;
        let _ = std::fs::create_dir(std::path::Path::new("/tmp/fs"));
        let python_command = vec!["python3", "python-e2e/ops.py"];
        let mut process = run(
            python_command,
            &service.pod_name,
            Some(&service.namespace),
            Some(vec!["--enable-fs", "--extract-path", "/tmp/fs"]),
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_remote_env_vars_exclude_works(#[future] service: EchoService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/remote_env/test_remote_env_vars_exclude_works.mjs",
        ];
        let mirrord_args = vec!["-x", "MIRRORD_FAKE_VAR_FIRST"];
        let mut process = run(node_command, &service.pod_name, None, Some(mirrord_args)).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_remote_env_vars_include_works(#[future] service: EchoService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/remote_env/test_remote_env_vars_include_works.mjs",
        ];
        let mirrord_args = vec!["-s", "MIRRORD_FAKE_VAR_FIRST"];
        let mut process = run(node_command, &service.pod_name, None, Some(mirrord_args)).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }
}
