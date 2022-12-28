#![feature(stmt_expr_attributes)]
mod env;
mod file_ops;
mod http;
mod pause;
mod target;
mod traffic;

#[cfg(test)]
mod utils {

    use std::{
        collections::HashMap,
        fmt::Debug,
        net::Ipv4Addr,
        process::Stdio,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use bytes::Bytes;
    use chrono::Utc;
    use futures::Stream;
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
        task::JoinHandle,
    };
    // 0.8
    use tokio_util::sync::{CancellationToken, DropGuard};

    static TEXT: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";
    pub const CONTAINER_NAME: &str = "test";

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

    #[derive(Debug)]
    pub enum Application {
        PythonFlaskHTTP,
        PythonFastApiHTTP,
        NodeHTTP,
        Go18HTTP,
        Go19HTTP,
    }

    #[derive(Debug)]
    pub enum Agent {
        #[cfg(target_os = "linux")]
        Ephemeral,
        Job,
    }

    #[derive(Debug)]
    pub enum FileOps {
        Python,
        Go18,
        Go19,
        Rust,
    }

    pub struct TestProcess {
        pub child: Child,
        stderr: Arc<Mutex<String>>,
        stdout: Arc<Mutex<String>>,
        // Keeps tempdir existing while process is running.
        _tempdir: TempDir,
    }

    impl TestProcess {
        pub fn get_stdout(&self) -> String {
            self.stdout.lock().unwrap().clone()
        }

        pub fn assert_stderr(&self) {
            assert!(self.stderr.lock().unwrap().is_empty());
        }

        pub fn assert_log_level(&self, stderr: bool, level: &str) {
            if stderr {
                assert!(!self.stderr.lock().unwrap().contains(level));
            } else {
                assert!(!self.stdout.lock().unwrap().contains(level));
            }
        }

        pub fn assert_python_fileops_stderr(&self) {
            assert!(!self.stderr.lock().unwrap().contains("FAILED"));
        }

        pub fn wait_for_line(&self, timeout: Duration, line: &str) {
            let now = std::time::Instant::now();
            while now.elapsed() < timeout {
                let stdout = self.get_stdout();
                if stdout.contains(line) {
                    return;
                }
            }
            panic!("Timeout waiting for line: {}", line);
        }

        pub fn from_child(mut child: Child, tempdir: TempDir) -> TestProcess {
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
                    eprintln!("stderr {:?} {pid}: {}", Utc::now(), string);
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
                    println!("stdout {:?} {pid}: {}", Utc::now(), string);
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
        pub fn get_cmd(&self) -> Vec<&str> {
            match self {
                Application::PythonFlaskHTTP => {
                    vec!["python3", "-u", "python-e2e/app_flask.py"]
                }
                Application::PythonFastApiHTTP => {
                    vec![
                        "uvicorn",
                        "--port=80",
                        "--host=0.0.0.0",
                        "--app-dir=./python-e2e/",
                        "app_fastapi:app",
                    ]
                }
                Application::NodeHTTP => vec!["node", "node-e2e/app.js"],
                Application::Go18HTTP => vec!["go-e2e/18"],
                Application::Go19HTTP => vec!["go-e2e/19"],
            }
        }

        pub async fn run(
            &self,
            target: &str,
            namespace: Option<&str>,
            args: Option<Vec<&str>>,
            env: Option<Vec<(&str, &str)>>,
        ) -> TestProcess {
            run_exec(self.get_cmd(), target, namespace, args, env).await
        }

        pub fn assert(&self, process: &TestProcess) {
            match self {
                Application::PythonFastApiHTTP => {
                    process.assert_log_level(true, "ERROR");
                    process.assert_log_level(false, "ERROR");
                    process.assert_log_level(true, "CRITICAL");
                    process.assert_log_level(false, "CRITICAL");
                }
                _ => process.assert_stderr(),
            }
        }
    }

    impl Agent {
        pub fn flag(&self) -> Option<Vec<&str>> {
            match self {
                #[cfg(target_os = "linux")]
                Agent::Ephemeral => Some(vec!["--ephemeral-container"]),
                Agent::Job => None,
            }
        }
    }

    impl FileOps {
        pub fn command(&self) -> Vec<&str> {
            match self {
                FileOps::Python => {
                    vec!["python3", "-B", "-m", "unittest", "-f", "python-e2e/ops.py"]
                }
                FileOps::Go18 => vec!["go-e2e-fileops/18"],
                FileOps::Go19 => vec!["go-e2e-fileops/19"],
                FileOps::Rust => vec!["../target/debug/rust-e2e-fileops"],
            }
        }

        pub fn assert(&self, process: TestProcess) {
            match self {
                FileOps::Python => process.assert_python_fileops_stderr(),
                _ => process.assert_stderr(),
            }
        }
    }

    pub async fn run_exec(
        process_cmd: Vec<&str>,
        target: &str,
        namespace: Option<&str>,
        args: Option<Vec<&str>>,
        env: Option<Vec<(&str, &str)>>,
    ) -> TestProcess {
        let path = match option_env!("MIRRORD_TESTS_USE_BINARY") {
            None => env!("CARGO_BIN_FILE_MIRRORD"),
            Some(binary_path) => binary_path,
        };
        let temp_dir = tempdir::TempDir::new("test").unwrap();
        let mut mirrord_args = vec!["exec", "--target", target, "-c"];
        if let Some(namespace) = namespace {
            mirrord_args.extend(["--target-namespace", namespace].into_iter());
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
        // docker build -t test . -f mirrord/agent/Dockerfile
        // minikube load image test:latest
        let mut base_env = HashMap::new();
        base_env.insert("MIRRORD_AGENT_IMAGE", "test");
        base_env.insert("MIRRORD_CHECK_VERSION", "false");
        base_env.insert("MIRRORD_AGENT_RUST_LOG", "warn,mirrord=trace");
        base_env.insert("MIRRORD_AGENT_COMMUNICATION_TIMEOUT", "180");
        base_env.insert("RUST_LOG", "warn,mirrord=trace");

        if let Some(env) = env {
            for (key, value) in env {
                base_env.insert(key, value);
            }
        }

        let server = Command::new(path)
            .args(args.clone())
            .envs(base_env)
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

    /// Runs `mirrord ls` command and asserts if the json matches the expected format
    pub fn run_ls(args: Option<Vec<&str>>, namespace: Option<&str>) -> TestProcess {
        let path = match option_env!("MIRRORD_TESTS_USE_BINARY") {
            None => env!("CARGO_BIN_FILE_MIRRORD"),
            Some(binary_path) => binary_path,
        };
        let temp_dir = tempdir::TempDir::new("test").unwrap();
        let mut mirrord_args = vec!["ls"];
        if let Some(args) = args {
            mirrord_args.extend(args);
        }
        if let Some(namespace) = namespace {
            mirrord_args.extend(vec!["--namespace", namespace]);
        }

        let process = Command::new(path)
            .args(mirrord_args.clone())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        println!(
            "executed mirrord with args {mirrord_args:?} pid {}",
            process.id().unwrap()
        );
        TestProcess::from_child(process, temp_dir)
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
        handle: JoinHandle<()>,
        delete_on_fail: bool,
    }

    impl ResourceGuard {
        /// Creates a resource and spawns a task to delete it when dropped
        /// I'm not sure why I have to add the `static here but this works?
        pub async fn create<K: Debug + Clone + DeserializeOwned + Serialize + 'static>(
            api: &Api<K>,
            name: String,
            data: &K,
            delete_on_fail: bool,
        ) -> ResourceGuard {
            api.create(&PostParams::default(), data).await.unwrap();
            let cancel_token = CancellationToken::new();
            let resource_token = cancel_token.clone();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let guard_barrier = barrier.clone();
            let name = name.clone();
            let cloned_api = api.clone();
            let handle = tokio::spawn(async move {
                cancel_token.cancelled().await;
                // Don't clean pods on failure, so that we can debug
                println!("deleting {:?}", &name);
                cloned_api
                    .delete(&name, &DeleteParams::default())
                    .await
                    .unwrap();
                barrier.wait();
            });
            Self {
                guard: Some(resource_token.drop_guard()),
                barrier: guard_barrier,
                handle,
                delete_on_fail,
            }
        }
    }

    impl Drop for ResourceGuard {
        fn drop(&mut self) {
            if !self.delete_on_fail && std::thread::panicking() {
                // If we're panicking and we shouldn't delete the resources on fail (to allow for
                // inspection) then abort the cleaning task.
                self.handle.abort();
            } else {
                let guard = self.guard.take();
                drop(guard);
                self.barrier.wait();
            }
        }
    }

    pub struct KubeService {
        pub name: String,
        pub namespace: String,
        pub target: String,
        _pod: ResourceGuard,
        _service: ResourceGuard,
    }

    /// randomize_name: should a random suffix be added to the end of resource names? e.g.
    ///                 for `echo-service`, should we create as `echo-service-ybtdb`.
    /// delete_after_fail: delete resources even if the test fails.
    #[fixture]
    pub async fn service(
        #[future] kube_client: Client,
        #[default("default")] namespace: &str,
        #[default("NodePort")] service_type: &str,
        #[default("ghcr.io/metalbear-co/mirrord-pytest:latest")] image: &str,
        #[default("http-echo")] service_name: &str,
        #[default(true)] randomize_name: bool,
        #[default(false)] delete_after_fail: bool,
    ) -> KubeService {
        let kube_client = kube_client.await;
        let deployment_api: Api<Deployment> = Api::namespaced(kube_client.clone(), namespace);
        let service_api: Api<Service> = Api::namespaced(kube_client.clone(), namespace);
        let name;
        if randomize_name {
            name = format!("{}-{}", service_name, random_string());
        } else {
            // if using non-random name, delete existing resources first.
            // Just continue if they don't exist.
            let _res = service_api
                .delete(service_name, &DeleteParams::default())
                .await;
            let _res = deployment_api
                .delete(service_name, &DeleteParams::default())
                .await;
            name = service_name.to_string();
        }

        let deployment: Deployment = serde_json::from_value(json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {
                "name": &name,
                "labels": {
                    "app": &name
                }
            },
            "spec": {
                "replicas": 1,
                "selector": {
                    "matchLabels": {
                        "app": &name
                    }
                },
                "template": {
                    "metadata": {
                        "labels": {
                            "app": &name
                        }
                    },
                    "spec": {
                        "containers": [
                            {
                                "name": &CONTAINER_NAME,
                                "image": &image,
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
                                    },
                                    {
                                        "name": "MIRRORD_FAKE_VAR_THIRD",
                                        "value": "foo=bar"
                                    }
                                ],
                            }
                        ]
                    }
                }
            }
        }))
        .unwrap();
        let pod_guard = ResourceGuard::create(
            &deployment_api,
            name.to_string(),
            &deployment,
            delete_after_fail,
        )
        .await;
        watch_resource_exists(&deployment_api, &name).await;

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
                "type": &service_type,
                "selector": {
                    "app": &name
                },
                "sessionAffinity": "None",
                "ports": [
                    {
                        "name": "udp",
                        "protocol": "UDP",
                        "port": 31415,
                    },
                    {
                        "name": "http",
                        "protocol": "TCP",
                        "port": 80,
                        "targetPort": 80,
                    },
                ]
            }
        }))
        .unwrap();

        let service_guard =
            ResourceGuard::create(&service_api, name.to_string(), &service, delete_after_fail)
                .await;
        watch_resource_exists(&service_api, "default").await;

        let target = get_pod_instance(&kube_client, &name, namespace)
            .await
            .unwrap();
        let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), namespace);
        await_condition(pod_api, &target, is_pod_running())
            .await
            .unwrap();

        KubeService {
            name: name.to_string(),
            namespace: namespace.to_string(),
            target: format!("pod/{}/container/{}", target, CONTAINER_NAME),
            _pod: pod_guard,
            _service: service_guard,
        }
    }

    /// Service that should only be reachable from inside the cluster, as a communication partner
    /// for testing outgoing traffic. If this service receives the application's messages, they
    /// must have been intercepted and forwarded via the agent to be sent from the impersonated pod.
    #[fixture]
    pub async fn udp_logger_service(#[future] kube_client: Client) -> KubeService {
        service(
            kube_client,
            "default",
            "ClusterIP",
            "ghcr.io/metalbear-co/mirrord-node-udp-logger:latest",
            "udp-logger",
            true,
            false,
        )
        .await
    }

    #[fixture]
    pub async fn http_logger_service(#[future] kube_client: Client) -> KubeService {
        service(
            kube_client,
            "default",
            "ClusterIP",
            "ghcr.io/metalbear-co/mirrord-http-logger:latest",
            "mirrord-tests-http-logger",
            false, // So that requester can reach logger by name.
            true,
        )
        .await
    }

    #[fixture]
    pub async fn http_log_requester_service(#[future] kube_client: Client) -> KubeService {
        service(
            kube_client,
            "default",
            "ClusterIP",
            "ghcr.io/metalbear-co/mirrord-http-log-requester:latest",
            "mirrord-http-log-requester",
            // Have a non-random name, so that there can only be one requester at any point in time
            // so that another requester does not send requests while this one is paused.
            false,
            true, // Delete also on fail, cause this service constantly does work.
        )
        .await
    }

    pub fn resolve_node_host() -> String {
        if (cfg!(target_os = "linux") && !wsl::is_wsl()) || std::env::var("USE_MINIKUBE").is_ok() {
            let output = std::process::Command::new("minikube")
                .arg("ip")
                .output()
                .unwrap()
                .stdout;
            String::from_utf8_lossy(&output).to_string()
        } else {
            // We assume it's either Docker for Mac or passed via wsl integration
            "127.0.0.1".to_string()
        }
    }

    pub async fn get_service_url(kube_client: Client, service: &KubeService) -> String {
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

    pub async fn send_requests(
        url: &str,
        expect_response: bool,
        headers: reqwest::header::HeaderMap,
    ) {
        // Create client for each request until we have a match between local app and remote app
        // as connection state is flaky
        println!("{url}");
        let client = reqwest::Client::new();
        let res = client
            .get(url)
            .headers(headers.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back

        let resp = res.bytes().await.unwrap();
        if expect_response {
            assert_eq!(resp, Bytes::from("GET"));
        }

        let client = reqwest::Client::new();
        let res = client
            .post(url)
            .headers(headers.clone())
            .body(TEXT)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back
        let resp = res.bytes().await.unwrap();
        if expect_response {
            assert_eq!(resp, "POST".as_bytes());
        }

        let client = reqwest::Client::new();
        let res = client
            .put(url)
            .headers(headers.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back
        let resp = res.bytes().await.unwrap();
        if expect_response {
            assert_eq!(resp, "PUT".as_bytes());
        }

        let client = reqwest::Client::new();
        let res = client.delete(url).headers(headers).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back
        let resp = res.bytes().await.unwrap();
        if expect_response {
            assert_eq!(resp, "DELETE".as_bytes());
        }
    }

    pub async fn get_next_log<T: Stream<Item = Result<Bytes, kube::Error>> + Unpin>(
        stream: &mut T,
    ) -> String {
        String::from_utf8_lossy(&stream.try_next().await.unwrap().unwrap()).to_string()
    }
}
