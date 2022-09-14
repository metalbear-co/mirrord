#![feature(stmt_expr_attributes)]
#[cfg(test)]

mod tests {
    use std::{
        collections::HashMap,
        fmt::Debug,
        net::{Ipv4Addr, UdpSocket},
        process::Stdio,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use bytes::Bytes;
    use chrono::Utc;
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

    #[derive(Debug)]
    enum Application {
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

        fn assert_log_level(&self, stderr: bool, level: &str) {
            if stderr {
                assert!(!self.stderr.lock().unwrap().contains(level));
            } else {
                assert!(!self.stdout.lock().unwrap().contains(level));
            }
        }

        fn assert_python_fileops_stderr(&self) {
            assert!(!self.stderr.lock().unwrap().contains("FAILED"));
        }

        fn wait_for_line(&self, timeout: Duration, line: &str) {
            let now = std::time::Instant::now();
            while now.elapsed() < timeout {
                let stdout = self.get_stdout();
                if stdout.contains(line) {
                    return;
                }
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
        async fn run(
            &self,
            pod_name: &str,
            namespace: Option<&str>,
            args: Option<Vec<&str>>,
        ) -> TestProcess {
            let process_cmd = match self {
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
            };
            run(process_cmd, pod_name, namespace, args).await
        }

        fn assert(&self, process: &TestProcess) {
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
        fn flag(&self) -> Option<Vec<&str>> {
            match self {
                #[cfg(target_os = "linux")]
                Agent::Ephemeral => Some(vec!["--ephemeral-container"]),
                Agent::Job => None,
            }
        }
    }

    impl FileOps {
        fn command(&self) -> Vec<&str> {
            match self {
                FileOps::Python => {
                    vec!["python3", "-B", "-m", "unittest", "-f", "python-e2e/ops.py"]
                }
                FileOps::Go18 => vec!["go-e2e-fileops/18"],
                FileOps::Go19 => vec!["go-e2e-fileops/19"],
            }
        }

        fn assert(&self, process: TestProcess) {
            match self {
                FileOps::Python => process.assert_python_fileops_stderr(),
                FileOps::Go18 => process.assert_stderr(),
                FileOps::Go19 => process.assert_stderr(),
            }
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
        env.insert("MIRRORD_AGENT_RUST_LOG", "warn,mirrord=debug");
        env.insert("MIRRORD_IMPERSONATED_CONTAINER_NAME", "test");
        env.insert("MIRRORD_AGENT_COMMUNICATION_TIMEOUT", "180");
        env.insert("RUST_LOG", "warn,mirrord=debug");
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
        handle: JoinHandle<()>,
    }

    impl ResourceGuard {
        /// Creates a resource and spawns a task to delete it when dropped
        /// I'm not sure why I have to add the `static here but this works?
        pub async fn create<K: Debug + Clone + DeserializeOwned + Serialize + 'static>(
            api: &Api<K>,
            name: String,
            data: &K,
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
            }
        }
    }

    impl Drop for ResourceGuard {
        fn drop(&mut self) {
            if std::thread::panicking() {
                self.handle.abort();
            } else {
                let guard = self.guard.take();
                drop(guard);
                self.barrier.wait();
            }
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
                                    },
                                    {
                                        "name": "MIRRORD_FAKE_VAR_THIRD",
                                        "value": "foo=bar"
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

        let pod_name = get_pod_instance(&kube_client, &name, namespace)
            .await
            .unwrap();
        let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), namespace);
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

    fn resolve_node_host() -> String {
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

    pub async fn send_requests(url: &str, expect_response: bool) {
        // Create client for each request until we have a match between local app and remote app
        // as connection state is flaky
        println!("{url}");
        let client = reqwest::Client::new();
        let res = client.get(url).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back

        let resp = res.bytes().await.unwrap();
        if expect_response {
            assert_eq!(resp, Bytes::from("GET"));
        }

        let client = reqwest::Client::new();
        let res = client.post(url).body(TEXT).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back
        let resp = res.bytes().await.unwrap();
        if expect_response {
            assert_eq!(resp, "POST".as_bytes());
        }

        let client = reqwest::Client::new();
        let res = client.put(url).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back
        let resp = res.bytes().await.unwrap();
        if expect_response {
            assert_eq!(resp, "PUT".as_bytes());
        }

        let client = reqwest::Client::new();
        let res = client.delete(url).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // read all data sent back
        let resp = res.bytes().await.unwrap();
        if expect_response {
            assert_eq!(resp, "DELETE".as_bytes());
        }
    }

    #[cfg(target_os = "linux")]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn test_mirror_http_traffic(
        #[future]
        #[notrace]
        service: EchoService,
        #[future]
        #[notrace]
        kube_client: Client,
        #[values(
            Application::NodeHTTP,
            Application::Go18HTTP,
            Application::Go19HTTP,
            Application::PythonFlaskHTTP,
            Application::PythonFastApiHTTP
        )]
        application: Application,
        #[values(Agent::Ephemeral, Agent::Job)] agent: Agent,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let mut process = application
            .run(&service.pod_name, Some(&service.namespace), agent.flag())
            .await;
        process.wait_for_line(Duration::from_secs(120), "daemon subscribed");
        send_requests(&url, false).await;
        process.wait_for_line(Duration::from_secs(10), "GET");
        process.wait_for_line(Duration::from_secs(10), "POST");
        process.wait_for_line(Duration::from_secs(10), "PUT");
        process.wait_for_line(Duration::from_secs(10), "DELETE");
        timeout(Duration::from_secs(40), process.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&process);
    }

    #[cfg(target_os = "macos")]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn test_mirror_http_traffic(
        #[future]
        #[notrace]
        service: EchoService,
        #[future]
        #[notrace]
        kube_client: Client,
        #[values(Application::PythonFlaskHTTP, Application::PythonFastApiHTTP)]
        application: Application,
        #[values(Agent::Job)] agent: Agent,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let mut process = application
            .run(&service.pod_name, Some(&service.namespace), agent.flag())
            .await;
        process.wait_for_line(Duration::from_secs(300), "daemon subscribed");
        send_requests(&url, false).await;
        process.wait_for_line(Duration::from_secs(10), "GET");
        process.wait_for_line(Duration::from_secs(10), "POST");
        process.wait_for_line(Duration::from_secs(10), "PUT");
        process.wait_for_line(Duration::from_secs(10), "DELETE");
        timeout(Duration::from_secs(40), process.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&process);
    }

    #[cfg(target_os = "linux")]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_file_ops(
        #[future]
        #[notrace]
        service: EchoService,
        #[values(Agent::Ephemeral, Agent::Job)] agent: Agent,
        #[values(FileOps::Python, FileOps::Go18, FileOps::Go19)] ops: FileOps,
    ) {
        let service = service.await;
        let _ = std::fs::create_dir(std::path::Path::new("/tmp/fs"));
        let command = ops.command();

        let mut args = vec!["--rw"];

        if let Some(ephemeral_flag) = agent.flag() {
            args.extend(ephemeral_flag);
        }

        let mut process = run(
            command,
            &service.pod_name,
            Some(&service.namespace),
            Some(args),
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        ops.assert(process);
    }

    #[cfg(target_os = "macos")]
    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_file_ops(
        #[future]
        #[notrace]
        service: EchoService,
        #[values(Agent::Job)] agent: Agent,
    ) {
        let service = service.await;
        let _ = std::fs::create_dir(std::path::Path::new("/tmp/fs"));
        let python_command = vec!["python3", "-B", "-m", "unittest", "-f", "python-e2e/ops.py"];
        let args = vec!["--rw"];
        let mut process = run(
            python_command,
            &service.pod_name,
            Some(&service.namespace),
            Some(args),
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_python_fileops_stderr();
    }

    #[rstest]
    #[trace]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_file_ops_ro(
        #[future]
        #[notrace]
        service: EchoService,
    ) {
        let service = service.await;
        let _ = std::fs::create_dir(std::path::Path::new("/tmp/fs"));
        let python_command = vec![
            "python3",
            "-B",
            "-m",
            "unittest",
            "-f",
            "python-e2e/files_ro.py",
        ];

        let mut process = run(
            python_command,
            &service.pod_name,
            Some(&service.namespace),
            None,
        )
        .await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_python_fileops_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
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
    #[timeout(Duration::from_secs(240))]
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

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_remote_dns_enabled_works(#[future] service: EchoService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/remote_dns/test_remote_dns_enabled_works.mjs",
        ];
        let mut process = run(node_command, &service.pod_name, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_remote_dns_lookup_google(#[future] service: EchoService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/remote_dns/test_remote_dns_lookup_google.mjs",
        ];
        let mut process = run(node_command, &service.pod_name, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    async fn test_steal_http_traffic(
        #[future] service: EchoService,
        #[future] kube_client: Client,
        #[values(
            Application::PythonFlaskHTTP,
            Application::PythonFastApiHTTP,
            Application::NodeHTTP
        )]
        application: Application,
        #[values(Agent::Ephemeral, Agent::Job)] agent: Agent,
    ) {
        let service = service.await;
        let kube_client = kube_client.await;
        let url = get_service_url(kube_client.clone(), &service).await;
        let mut flags = vec!["--steal"];
        agent.flag().map(|flag| flags.extend(flag));
        let mut process = application
            .run(&service.pod_name, Some(&service.namespace), Some(flags))
            .await;

        process.wait_for_line(Duration::from_secs(30), "daemon subscribed");
        send_requests(&url, true).await;
        timeout(Duration::from_secs(40), process.child.wait())
            .await
            .unwrap()
            .unwrap();

        application.assert(&process);
    }

    #[rstest]
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_bash_remote_env_vars_works(#[future] service: EchoService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/env.sh"];
        let mut process = run(bash_command, &service.pod_name, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_bash_remote_env_vars_exclude_works(#[future] service: EchoService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/env.sh", "exclude"];
        let mirrord_args = vec!["-x", "MIRRORD_FAKE_VAR_FIRST"];
        let mut process = run(bash_command, &service.pod_name, None, Some(mirrord_args)).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_bash_remote_env_vars_include_works(#[future] service: EchoService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/env.sh", "include"];
        let mirrord_args = vec!["-s", "MIRRORD_FAKE_VAR_FIRST"];
        let mut process = run(bash_command, &service.pod_name, None, Some(mirrord_args)).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    // Currently fails due to Layer >> AddressConversion in ci for some reason

    #[ignore]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_bash_file_exists(#[future] service: EchoService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/file.sh", "exists"];
        let mut process = run(bash_command, &service.pod_name, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    // currently there is an issue with piping across forks of processes so 'test_bash_file_read'
    // and 'test_bash_file_write' cannot pass

    #[ignore]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_bash_file_read(#[future] service: EchoService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/file.sh", "read"];
        let mut process = run(bash_command, &service.pod_name, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[ignore]
    #[cfg(target_os = "linux")]
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(240))]
    pub async fn test_bash_file_write(#[future] service: EchoService) {
        let service = service.await;
        let bash_command = vec!["bash", "bash-e2e/file.sh", "write"];
        let args = vec!["--rw"];
        let mut process = run(bash_command, &service.pod_name, None, Some(args)).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_go18_remote_env_vars_works(#[future] service: EchoService) {
        let service = service.await;
        let command = vec!["go-e2e-env/18"];
        let mut process = run(command, &service.pod_name, None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_go19_remote_env_vars_works(#[future] service: EchoService) {
        let service = service.await;
        let command = vec!["go-e2e-env/19"];
        let mut process = run(command, &service.pod_name, None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    // TODO: This is valid for all "outgoing" tests:
    // We have no way of knowing if they're actually being hooked, so they'll pass without mirrord,
    // which is bad.
    // An idea to solve this problem would be to have some internal (or test-only) specific messages
    // that we can pass back and forth between `layer` and `agent`.
    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_outgoing_traffic_single_request_enabled(#[future] service: EchoService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_single_request.mjs",
        ];
        let mut process = run(node_command, &service.pod_name, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_outgoing_traffic_single_request_disabled(#[future] service: EchoService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_single_request.mjs",
        ];
        let mirrord_args = vec!["--no-outgoing"];
        let mut process = run(node_command, &service.pod_name, None, Some(mirrord_args)).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_outgoing_traffic_many_requests_enabled(#[future] service: EchoService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_many_requests.mjs",
        ];
        let mut process = run(node_command, &service.pod_name, None, None).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_outgoing_traffic_many_requests_disabled(#[future] service: EchoService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_many_requests.mjs",
        ];
        let mirrord_args = vec!["--no-outgoing"];
        let mut process = run(node_command, &service.pod_name, None, Some(mirrord_args)).await;

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_outgoing_traffic_make_request_after_listen(#[future] service: EchoService) {
        let service = service.await;
        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_make_request_after_listen.mjs",
        ];
        let mut process = run(node_command, &service.pod_name, None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[timeout(Duration::from_secs(30))]
    pub async fn test_outgoing_traffic_udp(#[future] service: EchoService) {
        let service = service.await;
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port().to_string();

        let node_command = vec![
            "node",
            "node-e2e/outgoing/test_outgoing_traffic_udp_client.mjs",
            &port,
        ];
        let mut process = run(node_command, &service.pod_name, None, None).await;

        // Listen for UDP message from mirrorded app.
        let mut buf = [0; 27];
        let amt = socket.recv(&mut buf).unwrap();
        assert_eq!(amt, 27);
        assert_eq!(buf, "Can I pass the test please?".as_ref()); // Sure you can.

        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_go18_outgoing_traffic_single_request_enabled(#[future] service: EchoService) {
        let service = service.await;
        let command = vec!["go-e2e-outgoing/18"];
        let mut process = run(command, &service.pod_name, None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }

    #[rstest]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    pub async fn test_go19_outgoing_traffic_single_request_enabled(#[future] service: EchoService) {
        let service = service.await;
        let command = vec!["go-e2e-outgoing/19"];
        let mut process = run(command, &service.pod_name, None, None).await;
        let res = process.child.wait().await.unwrap();
        assert!(res.success());
        process.assert_stderr();
    }
}
