#![feature(stmt_expr_attributes)]
#![warn(clippy::indexing_slicing)]

mod env;
mod file_ops;
mod http;
mod pause;
mod target;
mod targetless;
mod traffic;

#[cfg(test)]
mod utils {
    use std::{
        collections::HashMap,
        fmt::Debug,
        net::Ipv4Addr,
        process::Stdio,
        sync::{Arc, Condvar, Mutex},
        time::Duration,
    };

    use chrono::Utc;
    use futures_util::stream::TryStreamExt;
    use k8s_openapi::api::{
        apps::v1::Deployment,
        core::v1::{Namespace, Pod, Service},
    };
    use kube::{
        api::{DeleteParams, ListParams, PostParams, WatchParams},
        core::WatchEvent,
        runtime::wait::{await_condition, conditions::is_pod_running},
        Api, Client, Config, Error,
    };
    use rand::{distributions::Alphanumeric, Rng};
    use reqwest::{RequestBuilder, StatusCode};
    use rstest::*;
    use serde::{de::DeserializeOwned, Serialize};
    use serde_json::json;
    use tempfile::{tempdir, TempDir};
    use tokio::{
        io::{AsyncReadExt, BufReader},
        process::{Child, Command},
        sync::oneshot::{self, Sender},
    };

    const TEXT: &'static str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";
    pub const CONTAINER_NAME: &str = "test";

    /// Name of the environment variable used to control cleanup after failed tests.
    /// By default, resources from failed tests are deleted.
    /// However, if this variable is set, resources will always be preserved.
    pub const PRESERVE_FAILED_ENV_NAME: &'static str = "MIRRORD_E2E_PRESERVE_FAILED";

    /// All Kubernetes resources created for testing purposes share this label.
    pub const TEST_RESOURCE_LABEL: (&'static str, &'static str) =
        ("mirrord-e2e-test-resource", "true");

    pub async fn watch_resource_exists<K: Debug + Clone + DeserializeOwned>(
        api: &Api<K>,
        name: &str,
    ) {
        let params = WatchParams::default()
            .fields(&format!("metadata.name={name}"))
            .timeout(10);
        let stream = api.watch(&params, "0").await.unwrap();
        tokio::pin!(stream);
        while let Some(status) = stream.try_next().await.unwrap() {
            match status {
                WatchEvent::Modified(_) => break,
                WatchEvent::Error(s) => {
                    panic!("Error watching namespaces: {s:?}");
                }
                _ => {}
            }
        }
    }

    /// Creates a random string of 7 alphanumeric lowercase characters.
    fn random_string() -> String {
        rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(7)
            .map(char::from)
            .collect::<String>()
            .to_ascii_lowercase()
    }

    #[derive(Debug)]
    #[allow(dead_code)]
    pub enum Application {
        PythonFlaskHTTP,
        PythonFastApiHTTP,
        NodeHTTP,
        NodeHTTP2,
        Go18HTTP,
        Go19HTTP,
        Go20HTTP,
        CurlToKubeApi,
    }

    #[derive(Debug)]
    pub enum Agent {
        Ephemeral,
        Job,
    }

    #[derive(Debug)]
    pub enum FileOps {
        #[cfg(target_os = "linux")]
        Python,
        #[cfg(target_os = "linux")]
        Rust,
        GoDir18,
        GoDir19,
        GoDir20,
    }

    #[derive(Debug)]
    pub enum EnvApp {
        Go18,
        Go19,
        Go20,
        Bash,
        BashInclude,
        BashExclude,
        NodeInclude,
        NodeExclude,
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

        pub fn get_stderr(&self) -> String {
            self.stderr.lock().unwrap().clone()
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
                let stderr = self.get_stderr();
                if stderr.contains(line) {
                    return;
                }
            }
            panic!("Timeout waiting for line: {line}");
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
                Application::NodeHTTP2 => {
                    vec!["node", "node-e2e/http2/test_http2_traffic_steal.mjs"]
                }
                Application::Go18HTTP => vec!["go-e2e/18.go_test_app"],
                Application::Go19HTTP => vec!["go-e2e/19.go_test_app"],
                Application::Go20HTTP => vec!["go-e2e/20.go_test_app"],
                Application::CurlToKubeApi => {
                    vec!["curl", "https://kubernetes/api", "--insecure"]
                }
            }
        }

        pub async fn run_targetless(
            &self,
            namespace: Option<&str>,
            args: Option<Vec<&str>>,
            env: Option<Vec<(&str, &str)>>,
        ) -> TestProcess {
            run_exec_targetless(self.get_cmd(), namespace, args, env).await
        }

        pub async fn run(
            &self,
            target: &str,
            namespace: Option<&str>,
            args: Option<Vec<&str>>,
            env: Option<Vec<(&str, &str)>>,
        ) -> TestProcess {
            run_exec_with_target(self.get_cmd(), target, namespace, args, env).await
        }

        pub fn assert(&self, process: &TestProcess) {
            match self {
                Application::PythonFastApiHTTP => {
                    process.assert_log_level(true, "ERROR");
                    process.assert_log_level(false, "ERROR");
                    process.assert_log_level(true, "CRITICAL");
                    process.assert_log_level(false, "CRITICAL");
                }
                _ => {}
            }
        }
    }

    impl Agent {
        pub fn flag(&self) -> Option<Vec<&str>> {
            match self {
                Agent::Ephemeral => Some(vec!["--ephemeral-container"]),
                Agent::Job => None,
            }
        }
    }

    impl FileOps {
        pub fn command(&self) -> Vec<&str> {
            match self {
                #[cfg(target_os = "linux")]
                FileOps::Python => {
                    vec!["python3", "-B", "-m", "unittest", "-f", "python-e2e/ops.py"]
                }
                #[cfg(target_os = "linux")]
                FileOps::Rust => vec!["../target/debug/rust-e2e-fileops"],
                FileOps::GoDir18 => vec!["go-e2e-dir/18.go_test_app"],
                FileOps::GoDir19 => vec!["go-e2e-dir/19.go_test_app"],
                FileOps::GoDir20 => vec!["go-e2e-dir/20.go_test_app"],
            }
        }

        #[cfg(target_os = "linux")]
        pub fn assert(&self, process: TestProcess) {
            match self {
                FileOps::Python => process.assert_python_fileops_stderr(),
                _ => {}
            }
        }
    }

    impl EnvApp {
        pub fn command(&self) -> Vec<&str> {
            match self {
                Self::Go18 => vec!["go-e2e-env/18.go_test_app"],
                Self::Go19 => vec!["go-e2e-env/19.go_test_app"],
                Self::Go20 => vec!["go-e2e-env/20.go_test_app"],
                Self::Bash => vec!["bash", "bash-e2e/env.sh"],
                Self::BashInclude => vec!["bash", "bash-e2e/env.sh", "include"],
                Self::BashExclude => vec!["bash", "bash-e2e/env.sh", "exclude"],
                Self::NodeInclude => vec![
                    "node",
                    "node-e2e/remote_env/test_remote_env_vars_include_works.mjs",
                ],
                Self::NodeExclude => vec![
                    "node",
                    "node-e2e/remote_env/test_remote_env_vars_exclude_works.mjs",
                ],
            }
        }

        pub fn mirrord_args(&self) -> Option<Vec<&str>> {
            match self {
                Self::BashInclude | Self::NodeInclude => Some(vec!["-s", "MIRRORD_FAKE_VAR_FIRST"]),
                Self::BashExclude | Self::NodeExclude => Some(vec!["-x", "MIRRORD_FAKE_VAR_FIRST"]),
                Self::Go18 | Self::Go19 | Self::Go20 | Self::Bash => None,
            }
        }
    }

    /// Run `mirrord exec` without specifying a target, to run in targetless mode.
    pub async fn run_exec_targetless(
        process_cmd: Vec<&str>,
        namespace: Option<&str>,
        args: Option<Vec<&str>>,
        env: Option<Vec<(&str, &str)>>,
    ) -> TestProcess {
        run_exec(process_cmd, None, namespace, args, env).await
    }

    /// See [`run_exec`].
    pub async fn run_exec_with_target(
        process_cmd: Vec<&str>,
        target: &str,
        namespace: Option<&str>,
        args: Option<Vec<&str>>,
        env: Option<Vec<(&str, &str)>>,
    ) -> TestProcess {
        run_exec(process_cmd, Some(target), namespace, args, env).await
    }

    /// Run `mirrord exec` with the given cmd, optional target (`None` for targetless), namespace,
    /// mirrord args, and env vars.
    pub async fn run_exec(
        process_cmd: Vec<&str>,
        target: Option<&str>,
        namespace: Option<&str>,
        args: Option<Vec<&str>>,
        env: Option<Vec<(&str, &str)>>,
    ) -> TestProcess {
        let path = match option_env!("MIRRORD_TESTS_USE_BINARY") {
            None => env!("CARGO_BIN_FILE_MIRRORD"),
            Some(binary_path) => binary_path,
        };
        let temp_dir = tempdir().unwrap();
        let mut mirrord_args = vec!["exec", "-c"];
        if let Some(target) = target {
            mirrord_args.extend(["--target", target].into_iter());
        }
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
        let temp_dir = tempdir().unwrap();
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

    /// RAII-style guard for deleting kube resources after tests.
    /// This guard spawns a background task to delete the kube resource.
    struct ResourceGuard {
        /// Used in the implementation of [`Drop`] for this struct to block until the background
        /// task exits.
        task_finished: Arc<(Mutex<bool>, Condvar)>,
        /// Used to inform the background task whether this guard was dropped during panic.
        panic_tx: Option<Sender<bool>>,
    }

    impl ResourceGuard {
        /// Create a kube resource and spawn a task to delete it when this guard is dropped.
        /// Return [`Error`] if creating the resource failed.
        pub async fn create<K: Debug + Clone + DeserializeOwned + Serialize + 'static>(
            api: Api<K>,
            name: String,
            data: &K,
            delete_on_fail: bool,
        ) -> Result<ResourceGuard, Error> {
            api.create(&PostParams::default(), data).await?;

            let (panic_tx, panic_rx) = oneshot::channel::<bool>();
            let task_finished = Arc::new((Mutex::new(false), Condvar::new()));
            let finished = task_finished.clone();

            tokio::spawn(async move {
                let panic = panic_rx.await.unwrap_or(true);

                if !panic || delete_on_fail {
                    println!("Deleting resource `{name}`",);
                    if let Err(e) = api.delete(&name, &DeleteParams::default()).await {
                        println!("Failed to delete resource `{name}`: {e:?}");
                    }
                }

                *finished.0.lock().expect("ResourceGuard lock poisoned") = true;
                finished.1.notify_one();
            });

            Ok(Self {
                task_finished,
                panic_tx: Some(panic_tx),
            })
        }
    }

    impl Drop for ResourceGuard {
        fn drop(&mut self) {
            let task_alive = self
                .panic_tx
                .take()
                .expect("ResourceGuard holds empty panic_tx")
                .send(std::thread::panicking())
                .is_ok();

            if task_alive {
                let lock = self
                    .task_finished
                    .0
                    .lock()
                    .expect("ResourceGuard lock poisoned");
                let _lock = self
                    .task_finished
                    .1
                    .wait_while(lock, |deleted| !*deleted)
                    .expect("ResourceGuard lock poisoned");
            }
        }
    }

    /// A service deployed to the kubernetes cluster.
    /// Service is meant as in "Microservice", not as in the Kubernetes resource called Service.
    /// This includes a Deployment resource, a Service resource and optionally a Namespace.
    pub struct KubeService {
        pub name: String,
        pub namespace: String,
        pub target: String,
        _pod: ResourceGuard,
        _service: ResourceGuard,
        _namespace: Option<ResourceGuard>,
    }

    /// Create a new [`KubeService`] and related Kubernetes resources. The resources will be deleted
    /// when the returned service is dropped, unless it is dropped during panic.
    /// This behavior can be changed, see [`FORCE_CLEANUP_ENV_NAME`].
    /// * `randomize_name` - whether a random suffix should be added to the end of the resource
    ///   names
    #[fixture]
    pub async fn service(
        #[default("default")] namespace: &str,
        #[default("NodePort")] service_type: &str,
        #[default("ghcr.io/metalbear-co/mirrord-pytest:latest")] image: &str,
        #[default("http-echo")] service_name: &str,
        #[default(true)] randomize_name: bool,
        #[future] kube_client: Client,
    ) -> KubeService {
        let delete_after_fail = std::env::var_os(PRESERVE_FAILED_ENV_NAME).is_none();

        println!(
            "{:?} creating service {service_name:?} in namespace {namespace:?}",
            Utc::now()
        );

        let kube_client = kube_client.await;
        let namespace_api: Api<Namespace> = Api::all(kube_client.clone());
        let deployment_api: Api<Deployment> = Api::namespaced(kube_client.clone(), namespace);
        let service_api: Api<Service> = Api::namespaced(kube_client.clone(), namespace);

        let name = if randomize_name {
            format!("{}-{}", service_name, random_string())
        } else {
            // If using a non-random name, delete existing resources first.
            // Just continue if they don't exist.
            let _ = service_api
                .delete(service_name, &DeleteParams::default())
                .await;
            let _ = deployment_api
                .delete(service_name, &DeleteParams::default())
                .await;

            service_name.to_string()
        };

        let namespace_resource: Namespace = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Namespace",
            "metadata": {
                "name": namespace,
                "labels": {
                    TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
                }
            },
        }))
        .unwrap();
        // Create namespace and wrap it in ResourceGuard if it does not yet exist.
        let namespace_guard = ResourceGuard::create(
            namespace_api.clone(),
            namespace.to_string(),
            &namespace_resource,
            delete_after_fail,
        )
        .await
        .ok();
        if namespace_guard.is_some() {
            watch_resource_exists(&namespace_api, namespace).await;
        }

        let deployment: Deployment = serde_json::from_value(json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {
                "name": &name,
                "labels": {
                    "app": &name,
                    TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
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
            deployment_api.clone(),
            name.to_string(),
            &deployment,
            delete_after_fail,
        )
        .await
        .unwrap();
        watch_resource_exists(&deployment_api, &name).await;

        let service: Service = serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": {
                "name": &name,
                "labels": {
                    "app": &name,
                    TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
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
        let service_guard = ResourceGuard::create(
            service_api.clone(),
            name.clone(),
            &service,
            delete_after_fail,
        )
        .await
        .unwrap();
        watch_resource_exists(&service_api, "default").await;

        let target = get_pod_instance(kube_client.clone(), &name, namespace)
            .await
            .unwrap();
        let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), namespace);
        await_condition(pod_api, &target, is_pod_running())
            .await
            .unwrap();

        println!(
            "{:?} done creating service {service_name:?} in namespace {namespace:?}",
            Utc::now()
        );

        KubeService {
            name,
            namespace: namespace.to_string(),
            target: format!("pod/{target}/container/{CONTAINER_NAME}"),
            _pod: pod_guard,
            _service: service_guard,
            _namespace: namespace_guard,
        }
    }

    /// Service that should only be reachable from inside the cluster, as a communication partner
    /// for testing outgoing traffic. If this service receives the application's messages, they
    /// must have been intercepted and forwarded via the agent to be sent from the impersonated pod.
    #[fixture]
    pub async fn udp_logger_service(#[future] kube_client: Client) -> KubeService {
        service(
            "default",
            "ClusterIP",
            "ghcr.io/metalbear-co/mirrord-node-udp-logger:latest",
            "udp-logger",
            true,
            kube_client,
        )
        .await
    }

    #[fixture]
    pub async fn http_logger_service(#[future] kube_client: Client) -> KubeService {
        service(
            "default",
            "ClusterIP",
            "ghcr.io/metalbear-co/mirrord-http-logger:latest",
            "mirrord-tests-http-logger",
            false, // So that requester can reach logger by name.
            kube_client,
        )
        .await
    }

    #[fixture]
    pub async fn http_log_requester_service(#[future] kube_client: Client) -> KubeService {
        service(
            "default",
            "ClusterIP",
            "ghcr.io/metalbear-co/mirrord-http-log-requester:latest",
            "mirrord-http-log-requester",
            // Have a non-random name, so that there can only be one requester at any point in time
            // so that another requester does not send requests while this one is paused.
            false,
            kube_client,
        )
        .await
    }

    /// Service that listens on port 80 and returns "remote: <DATA>" when getting "<DATA>" directly
    /// over TCP, not HTTP.
    #[fixture]
    pub async fn tcp_echo_service(#[future] kube_client: Client) -> KubeService {
        service(
            "default",
            "NodePort",
            "ghcr.io/metalbear-co/mirrord-tcp-echo:latest",
            "tcp-echo",
            true,
            kube_client,
        )
        .await
    }

    /// [Service](https://github.com/metalbear-co/test-images/blob/main/websocket/app.mjs)
    /// that listens on port 80 and returns "remote: <DATA>" when getting "<DATA>" over a websocket
    /// connection, allowing us to test HTTP upgrade requests.
    #[fixture]
    pub async fn websocket_service(#[future] kube_client: Client) -> KubeService {
        service(
            "default",
            "NodePort",
            "ghcr.io/metalbear-co/mirrord-websocket:latest",
            "websocket",
            true,
            kube_client,
        )
        .await
    }

    #[fixture]
    pub async fn http2_service(#[future] kube_client: Client) -> KubeService {
        service(
            "default",
            "NodePort",
            "ghcr.io/metalbear-co/mirrord-pytest:latest",
            "http2-echo",
            true,
            kube_client,
        )
        .await
    }

    /// Service that listens on port 80 and returns "remote: <DATA>" when getting "<DATA>" directly
    /// over TCP, not HTTP.
    #[fixture]
    pub async fn hostname_service(#[future] kube_client: Client) -> KubeService {
        service(
            "default",
            "NodePort",
            "ghcr.io/metalbear-co/mirrord-pytest:latest",
            "hostname-echo",
            true,
            kube_client,
        )
        .await
    }

    #[fixture]
    pub async fn random_namespace_self_deleting_service(
        #[future] kube_client: Client,
    ) -> KubeService {
        let namespace = format!("random-namespace-{}", random_string());
        service(
            &namespace,
            "NodePort",
            "ghcr.io/metalbear-co/mirrord-pytest:latest",
            "pytest-echo",
            true,
            kube_client,
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

    pub async fn get_service_host_and_port(
        kube_client: Client,
        service: &KubeService,
    ) -> (String, i32) {
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
        (host_ip, port.node_port.unwrap())
    }

    pub async fn get_service_url(kube_client: Client, service: &KubeService) -> String {
        let (host_ip, port) = get_service_host_and_port(kube_client, service).await;
        format!("http://{host_ip}:{port}")
    }

    /// Returns a name of any pod belonging to the given app.
    pub async fn get_pod_instance(
        client: Client,
        app_name: &str,
        namespace: &str,
    ) -> Option<String> {
        let pod_api: Api<Pod> = Api::namespaced(client, namespace);
        pod_api
            .list(&ListParams::default().labels(&format!("app={app_name}")))
            .await
            .unwrap()
            .into_iter()
            .find_map(|pod| pod.metadata.name)
    }

    /// Take a request builder of any method, add headers, send the request, verify success, and
    /// optionally verify expected response.
    pub async fn send_request(
        request_builder: RequestBuilder,
        expect_response: Option<&str>,
        headers: reqwest::header::HeaderMap,
    ) {
        let res = request_builder.headers(headers).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        if let Some(expected_response) = expect_response {
            let resp = res.bytes().await.unwrap();
            assert_eq!(resp, expected_response.as_bytes());
        }
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
        let req_builder = client.get(url);
        send_request(
            req_builder,
            expect_response.then_some("GET"),
            headers.clone(),
        )
        .await;

        let client = reqwest::Client::new();
        let req_builder = client.post(url).body(TEXT);
        send_request(
            req_builder,
            expect_response.then_some("POST"),
            headers.clone(),
        )
        .await;

        let client = reqwest::Client::new();
        let req_builder = client.put(url);
        send_request(
            req_builder,
            expect_response.then_some("PUT"),
            headers.clone(),
        )
        .await;

        let client = reqwest::Client::new();
        let req_builder = client.delete(url);
        send_request(
            req_builder,
            expect_response.then_some("DELETE"),
            headers.clone(),
        )
        .await;
    }
}
