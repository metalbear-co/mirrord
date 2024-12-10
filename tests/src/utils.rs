#![allow(clippy::unused_io_amount)]
#![allow(clippy::indexing_slicing)]

use std::{
    collections::HashMap,
    fmt::Debug,
    net::IpAddr,
    ops::Not,
    path::PathBuf,
    process::{ExitStatus, Stdio},
    sync::{Arc, Once},
    time::Duration,
};

use chrono::{Timelike, Utc};
use fancy_regex::Regex;
use futures::FutureExt;
use futures_util::{future::BoxFuture, stream::TryStreamExt};
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
use serde_json::{json, Value};
use tempfile::{tempdir, TempDir};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::RwLock,
    task::JoinHandle,
};

pub mod sqs_resources;

const TEXT: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";
pub const CONTAINER_NAME: &str = "test";

/// Name of the environment variable used to control cleanup after failed tests.
/// By default, resources from failed tests are deleted.
/// However, if this variable is set, resources will always be preserved.
pub const PRESERVE_FAILED_ENV_NAME: &str = "MIRRORD_E2E_PRESERVE_FAILED";

/// All Kubernetes resources created for testing purposes share this label.
pub const TEST_RESOURCE_LABEL: (&str, &str) = ("mirrord-e2e-test-resource", "true");

pub async fn watch_resource_exists<K: Debug + Clone + DeserializeOwned>(api: &Api<K>, name: &str) {
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
pub(crate) fn random_string() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(7)
        .map(char::from)
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Returns string with time format of hh:mm:ss
fn format_time() -> String {
    let now = Utc::now();
    format!("{:02}:{:02}:{:02}", now.hour(), now.minute(), now.second())
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum Application {
    PythonFlaskHTTP,
    PythonFastApiHTTP,
    PythonFastApiHTTPIPv6,
    NodeHTTP,
    NodeHTTP2,
    Go21HTTP,
    Go22HTTP,
    Go23HTTP,
    CurlToKubeApi,
    PythonCloseSocket,
    PythonCloseSocketKeepConnection,
    RustWebsockets,
    RustSqs,
}

#[derive(Debug)]
pub enum FileOps {
    #[cfg(target_os = "linux")]
    Python,
    #[cfg(target_os = "linux")]
    Rust,
    GoDir21,
    GoDir22,
    GoDir23,
}

#[derive(Debug)]
pub enum EnvApp {
    Go21,
    Go22,
    Go23,
    Bash,
    BashInclude,
    BashExclude,
    NodeInclude,
    NodeExclude,
}

pub struct TestProcess {
    pub child: Child,
    stderr_data: Arc<RwLock<String>>,
    stdout_data: Arc<RwLock<String>>,
    stderr_task: Option<JoinHandle<()>>,
    stdout_task: Option<JoinHandle<()>>,
    error_capture: Regex,
    warn_capture: Regex,
    // Keeps tempdir existing while process is running.
    _tempdir: Option<TempDir>,
}

impl TestProcess {
    pub async fn get_stdout(&self) -> String {
        self.stdout_data.read().await.clone()
    }

    pub async fn get_stderr(&self) -> String {
        self.stderr_data.read().await.clone()
    }

    pub async fn assert_log_level(&self, stderr: bool, level: &str) {
        if stderr {
            assert!(!self.stderr_data.read().await.contains(level));
        } else {
            assert!(!self.stdout_data.read().await.contains(level));
        }
    }

    pub async fn assert_python_fileops_stderr(&self) {
        assert!(!self.stderr_data.read().await.contains("FAILED"));
    }

    pub async fn wait_assert_success(&mut self) {
        let output = self.wait().await;
        assert!(output.success());
    }

    pub async fn wait_assert_fail(&mut self) {
        let output = self.wait().await;
        assert!(!output.success());
    }

    pub async fn assert_stdout_contains(&self, string: &str) {
        assert!(self.get_stdout().await.contains(string));
    }

    pub async fn assert_stdout_doesnt_contain(&self, string: &str) {
        assert!(!self.get_stdout().await.contains(string));
    }

    pub async fn assert_stderr_contains(&self, string: &str) {
        assert!(self.get_stderr().await.contains(string));
    }

    pub async fn assert_stderr_doesnt_contain(&self, string: &str) {
        assert!(!self.get_stderr().await.contains(string));
    }

    pub async fn assert_no_error_in_stdout(&self) {
        assert!(!self
            .error_capture
            .is_match(&self.stdout_data.read().await)
            .unwrap());
    }

    pub async fn assert_no_error_in_stderr(&self) {
        assert!(!self
            .error_capture
            .is_match(&self.stderr_data.read().await)
            .unwrap());
    }

    pub async fn assert_no_warn_in_stdout(&self) {
        assert!(!self
            .warn_capture
            .is_match(&self.stdout_data.read().await)
            .unwrap());
    }

    pub async fn assert_no_warn_in_stderr(&self) {
        assert!(!self
            .warn_capture
            .is_match(&self.stderr_data.read().await)
            .unwrap());
    }

    pub async fn wait_for_line(&self, timeout: Duration, line: &str) {
        let now = std::time::Instant::now();
        while now.elapsed() < timeout {
            let stderr = self.get_stderr().await;
            if stderr.contains(line) {
                return;
            }
            // avoid busyloop
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("Timeout waiting for line: {line}");
    }

    /// Wait for the test app to output (at least) the given amount of lines.
    ///
    /// # Arguments
    ///
    /// * `timeout` - how long to wait for process to output enough lines.
    ///
    /// # Panics
    /// If `timeout` has passed and stdout still does not contain `n` lines.
    pub async fn await_n_lines(&self, n: usize, timeout: Duration) -> Vec<String> {
        tokio::time::timeout(timeout, async move {
            loop {
                let stdout = self.get_stdout().await;
                if stdout.lines().count() >= n {
                    return stdout
                        .lines()
                        .map(ToString::to_string)
                        .collect::<Vec<String>>();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("Test process output did not produce expected amount of lines in time.")
    }

    /// Wait for the test app to output the given amount of lines, then assert it did not output
    /// more than expected.
    ///
    /// > Note: we do not wait to make sure more lines are not printed, we just check the lines we
    /// > got after waiting for n lines. So it is possible for this to happen:
    ///   1. Test process outputs `n` lines.
    ///   2. `await_n_lines` returns those `n` lines.
    ///   3. This function asserts there are only `n` lines.
    ///   3. The test process outputs more lines.
    ///
    /// # Arguments
    ///
    /// * `timeout` - how long to wait for process to output enough lines.
    ///
    /// # Panics
    /// - If `timeout` has passed and stdout still does not contain `n` lines.
    /// - If stdout contains more than `n` lines.
    pub async fn await_exactly_n_lines(&self, n: usize, timeout: Duration) -> Vec<String> {
        let lines = self.await_n_lines(n, timeout).await;
        assert_eq!(
            lines.len(),
            n,
            "Test application printed out more lines than expected."
        );
        lines
    }

    pub async fn write_to_stdin(&mut self, data: &[u8]) {
        if let Some(ref mut stdin) = self.child.stdin {
            stdin.write(data).await.unwrap();
        } else {
            panic!("Can't write to test app's stdin!");
        }
    }

    /// Waits for process to end and stdout/err to be read. - returns final exit status
    pub async fn wait(&mut self) -> ExitStatus {
        eprintln!("waiting for process to exit");
        let exit_status = self.child.wait().await.unwrap();
        eprintln!("process exit, waiting for stdout/err to be read");
        self.stdout_task
            .take()
            .expect("can't call wait twice")
            .await
            .unwrap();
        self.stderr_task
            .take()
            .expect("can't call wait twice")
            .await
            .unwrap();
        eprintln!("stdout/err read and finished");
        exit_status
    }

    pub fn from_child(mut child: Child, tempdir: Option<TempDir>) -> TestProcess {
        let stderr_data = Arc::new(RwLock::new(String::new()));
        let stdout_data = Arc::new(RwLock::new(String::new()));
        let child_stderr = child.stderr.take().unwrap();
        let child_stdout = child.stdout.take().unwrap();
        let stderr_data_reader = stderr_data.clone();
        let stdout_data_reader = stdout_data.clone();
        let pid = child.id().unwrap();

        let stderr_task = Some(tokio::spawn(async move {
            let mut reader = BufReader::new(child_stderr);
            let mut buf = [0; 1024];
            loop {
                let n = reader.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }

                let string = String::from_utf8_lossy(&buf[..n]);
                eprintln!("stderr {} {pid}: {}", format_time(), string);
                {
                    stderr_data_reader.write().await.push_str(&string);
                }
            }
        }));
        let stdout_task = Some(tokio::spawn(async move {
            let mut reader = BufReader::new(child_stdout);
            let mut buf = [0; 1024];
            loop {
                let n = reader.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                let string = String::from_utf8_lossy(&buf[..n]);
                print!("stdout {} {pid}: {}", format_time(), string);
                {
                    stdout_data_reader.write().await.push_str(&string);
                }
            }
        }));

        let error_capture = Regex::new(r"^.*ERROR[^\w_-]").unwrap();
        let warn_capture = Regex::new(r"WARN").unwrap();

        TestProcess {
            child,
            error_capture,
            warn_capture,
            stderr_data,
            stdout_data,
            stderr_task,
            stdout_task,
            _tempdir: tempdir,
        }
    }

    pub async fn start_process(
        executable: String,
        args: Vec<String>,
        env: HashMap<&str, &str>,
    ) -> TestProcess {
        println!("EXECUTING: {executable}");
        let child = Command::new(executable)
            .args(args)
            .envs(env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        println!("Started application.");
        TestProcess::from_child(child, None)
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
            Application::PythonFastApiHTTPIPv6 => {
                vec![
                    "uvicorn",
                    "--port=80",
                    "--host=::",
                    "--app-dir=./python-e2e/",
                    "app_fastapi:app",
                ]
            }
            Application::PythonCloseSocket => {
                vec!["python3", "-u", "python-e2e/close_socket.py"]
            }
            Application::PythonCloseSocketKeepConnection => {
                vec![
                    "python3",
                    "-u",
                    "python-e2e/close_socket_keep_connection.py",
                ]
            }
            Application::NodeHTTP => vec!["node", "node-e2e/app.js"],
            Application::NodeHTTP2 => {
                vec!["node", "node-e2e/http2/test_http2_traffic_steal.mjs"]
            }
            Application::Go21HTTP => vec!["go-e2e/21.go_test_app"],
            Application::Go22HTTP => vec!["go-e2e/22.go_test_app"],
            Application::Go23HTTP => vec!["go-e2e/23.go_test_app"],
            Application::CurlToKubeApi => {
                vec!["curl", "https://kubernetes/api", "--insecure"]
            }
            Application::RustWebsockets => vec!["../target/debug/rust-websockets"],
            Application::RustSqs => vec!["../target/debug/rust-sqs-printer"],
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

    pub async fn assert(&self, process: &TestProcess) {
        if matches!(self, Self::PythonFastApiHTTP | Self::PythonFastApiHTTPIPv6) {
            process.assert_log_level(true, "ERROR").await;
            process.assert_log_level(false, "ERROR").await;
            process.assert_log_level(true, "CRITICAL").await;
            process.assert_log_level(false, "CRITICAL").await;
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
            FileOps::GoDir21 => vec!["go-e2e-dir/21.go_test_app"],
            FileOps::GoDir22 => vec!["go-e2e-dir/22.go_test_app"],
            FileOps::GoDir23 => vec!["go-e2e-dir/23.go_test_app"],
        }
    }

    #[cfg(target_os = "linux")]
    pub async fn assert(&self, process: TestProcess) {
        if let FileOps::Python = self {
            process.assert_python_fileops_stderr().await
        }
    }
}

impl EnvApp {
    pub fn command(&self) -> Vec<&str> {
        match self {
            Self::Go21 => vec!["go-e2e-env/21.go_test_app"],
            Self::Go22 => vec!["go-e2e-env/22.go_test_app"],
            Self::Go23 => vec!["go-e2e-env/23.go_test_app"],
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
            Self::Go21 | Self::Go22 | Self::Go23 | Self::Bash => None,
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

pub async fn run_mirrord(args: Vec<&str>, env: HashMap<&str, &str>) -> TestProcess {
    let path = match option_env!("MIRRORD_TESTS_USE_BINARY") {
        None => env!("CARGO_BIN_FILE_MIRRORD"),
        Some(binary_path) => binary_path,
    };
    let temp_dir = tempdir().unwrap();

    let server = Command::new(path)
        .args(&args)
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    println!(
        "executed mirrord with args {args:?} pid {}",
        server.id().unwrap()
    );
    // We need to hold temp dir until the process is finished
    TestProcess::from_child(server, Some(temp_dir))
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
    // TODO: revert
    // base_env.insert("MIRRORD_AGENT_IMAGE", "test");
    base_env.insert("MIRRORD_AGENT_IMAGE", "docker.io/t4lz/mirrord-agent:latest");
    base_env.insert("MIRRORD_AGENT_TTL", "180"); // TODO: delete
    base_env.insert("MIRRORD_CHECK_VERSION", "false");
    base_env.insert("MIRRORD_AGENT_RUST_LOG", "warn,mirrord=debug");
    base_env.insert("MIRRORD_AGENT_COMMUNICATION_TIMEOUT", "180");
    base_env.insert("RUST_LOG", "warn,mirrord=debug");

    if let Some(env) = env {
        for (key, value) in env {
            base_env.insert(key, value);
        }
    }

    run_mirrord(args, base_env).await
}

/// Runs `mirrord ls` command and asserts if the json matches the expected format
pub async fn run_ls<const USE_OPERATOR: bool>(
    args: Option<Vec<&str>>,
    namespace: Option<&str>,
) -> TestProcess {
    let mut mirrord_args = vec!["ls"];
    if let Some(args) = args {
        mirrord_args.extend(args);
    }
    if let Some(namespace) = namespace {
        mirrord_args.extend(vec!["--namespace", namespace]);
    }

    let mut env = HashMap::new();
    if USE_OPERATOR {
        env.insert("MIRRORD_OPERATOR_ENABLE", "true");
    };

    run_mirrord(mirrord_args, env).await
}

/// Runs `mirrord verify-config [--ide] "/path/config.json"`.
///
/// ## Attention
///
/// The `verify-config` tests are here to guarantee that your changes do not break backwards
/// compatability, so you should not modify them, only add new tests (unless breakage is
/// wanted/required).
pub async fn run_verify_config(args: Option<Vec<&str>>) -> TestProcess {
    let mut mirrord_args = vec!["verify-config"];
    if let Some(args) = args {
        mirrord_args.extend(args);
    }

    run_mirrord(mirrord_args, Default::default()).await
}

static CRYPTO_PROVIDER_INSTALLED: Once = Once::new();

#[fixture]
pub async fn kube_client() -> Client {
    CRYPTO_PROVIDER_INSTALLED.call_once(|| {
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .expect("Failed to install crypto provider");
    });

    let mut config = Config::infer().await.unwrap();
    config.accept_invalid_certs = true;
    Client::try_from(config).unwrap()
}

/// RAII-style guard for deleting kube resources after tests.
/// This guard deletes the kube resource when dropped.
/// This guard can be configured not to delete the resource if dropped during a panic.
pub(crate) struct ResourceGuard {
    /// Whether the resource should be deleted if the test has panicked.
    delete_on_fail: bool,
    /// This future will delete the resource once awaited.
    deleter: Option<BoxFuture<'static, ()>>,
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

        let deleter = async move {
            println!("Deleting resource `{name}`");
            let delete_params = DeleteParams {
                grace_period_seconds: Some(0),
                ..Default::default()
            };
            let res = api.delete(&name, &delete_params).await;
            if let Err(e) = res {
                println!("Failed to delete resource `{name}`: {e:?}");
            }
        };

        Ok(Self {
            delete_on_fail,
            deleter: Some(deleter.boxed()),
        })
    }

    /// If the underlying resource should be deleted (e.g. current thread is not panicking or
    /// `delete_on_fail` was set), return the future to delete it. The future will
    /// delete the resource once awaited.
    ///
    /// Used to run deleters from multiple [`ResourceGuard`]s on one runtime.
    pub fn take_deleter(&mut self) -> Option<BoxFuture<'static, ()>> {
        if !std::thread::panicking() || self.delete_on_fail {
            self.deleter.take()
        } else {
            None
        }
    }
}

impl Drop for ResourceGuard {
    fn drop(&mut self) {
        let Some(deleter) = self.deleter.take() else {
            return;
        };

        if !std::thread::panicking() || self.delete_on_fail {
            let _ = std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to create tokio runtime")
                    .block_on(deleter);
            })
            .join();
        }
    }
}

/// A service deployed to the kubernetes cluster.
///
/// Service is meant as in "Microservice", not as in the Kubernetes resource called Service.
/// This includes a Deployment resource, a Service resource and optionally a Namespace.
pub struct KubeService {
    pub name: String,
    pub namespace: String,
    pub target: String,
    guards: Vec<ResourceGuard>,
    namespace_guard: Option<ResourceGuard>,
}

impl KubeService {
    pub fn deployment_target(&self) -> String {
        format!("deployment/{}", self.name)
    }
}

impl Drop for KubeService {
    fn drop(&mut self) {
        let mut deleters = self
            .guards
            .iter_mut()
            .map(ResourceGuard::take_deleter)
            .collect::<Vec<_>>();

        deleters.push(
            self.namespace_guard
                .as_mut()
                .and_then(ResourceGuard::take_deleter),
        );

        let deleters = deleters.into_iter().flatten().collect::<Vec<_>>();

        if deleters.is_empty() {
            return;
        }

        let _ = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to create tokio runtime")
                .block_on(futures::future::join_all(deleters));
        })
        .join();
    }
}

fn deployment_from_json(name: &str, image: &str, env: Value) -> Deployment {
    serde_json::from_value(json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {
            "name": name,
            "labels": {
                "app": name,
                TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
                "test-label-for-deployments": format!("deployment-{name}")
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
                        "app": &name,
                        "test-label-for-pods": format!("pod-{name}"),
                        format!("test-label-for-pods-{name}"): &name
                    }
                },
                "spec": {
                    "containers": [
                        {
                            "name": &CONTAINER_NAME,
                            "image": image,
                            "ports": [
                                {
                                    "containerPort": 80
                                }
                            ],
                            "env": env,
                        }
                    ]
                }
            }
        }
    }))
    .expect("Failed creating `deployment` from json spec!")
}

/// Change the `ipFamilies` and `ipFamilyPolicy` fields to make the service IPv6-only.
///
/// # Panics
///
/// Will panic if the given service does not have a spec.
fn set_ipv6_only(service: &mut Service) {
    let spec = service.spec.as_mut().unwrap();
    spec.ip_families = Some(vec!["IPv6".to_string()]);
    spec.ip_family_policy = Some("SingleStack".to_string());
}

fn service_from_json(name: &str, service_type: &str) -> Service {
    serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {
            "name": name,
            "labels": {
                "app": name,
                TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
            }
        },
        "spec": {
            "type": service_type,
            "selector": {
                "app": name
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
    .expect("Failed creating `service` from json spec!")
}

#[cfg(feature = "operator")]
fn stateful_set_from_json(name: &str, image: &str) -> k8s_openapi::api::apps::v1::StatefulSet {
    serde_json::from_value(json!({
        "apiVersion": "apps/v1",
        "kind": "StatefulSet",
        "metadata": {
            "name": name,
            "labels": {
                "app": name,
                TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
                "test-label-for-statefulsets": format!("statefulset-{name}")
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
                        "app": &name,
                        "test-label-for-pods": format!("pod-{name}"),
                        format!("test-label-for-pods-{name}"): &name
                    }
                },
                "spec": {
                    "containers": [
                        {
                            "name": &CONTAINER_NAME,
                            "image": image,
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
    .expect("Failed creating `statefulset` from json spec!")
}

#[cfg(feature = "operator")]
fn cron_job_from_json(name: &str, image: &str) -> k8s_openapi::api::batch::v1::CronJob {
    serde_json::from_value(json!({
        "apiVersion": "batch/v1",
        "kind": "CronJob",
        "metadata": {
            "name": name,
            "labels": {
                "app": name,
                TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
                "test-label-for-cronjobs": format!("cronjob-{name}")
            }
        },
        "spec": {
            "schedule": "* * * * *",
            "concurrencyPolicy": "Forbid",
            "jobTemplate": {
                "metadata": {
                    "labels": {
                        "app": &name,
                        "test-label-for-pods": format!("pod-{name}"),
                        format!("test-label-for-pods-{name}"): &name
                    }
                },
                "spec": {
                    "template": {
                        "spec": {
                            "restartPolicy": "OnFailure",
                            "containers": [
                                {
                                    "name": &CONTAINER_NAME,
                                    "image": image,
                                    "ports": [{ "containerPort": 80 }],
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
            }
        }
    }))
    .expect("Failed creating `cronjob` from json spec!")
}

#[cfg(feature = "operator")]
fn job_from_json(name: &str, image: &str) -> k8s_openapi::api::batch::v1::Job {
    serde_json::from_value(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": name,
            "labels": {
                "app": name,
                TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
                "test-label-for-jobs": format!("job-{name}")
            }
        },
        "spec": {
            "ttlSecondsAfterFinished": 10,
            "backoffLimit": 1,
            "template": {
                "metadata": {
                    "labels": {
                        "app": &name,
                        "test-label-for-pods": format!("pod-{name}"),
                        format!("test-label-for-pods-{name}"): &name
                    }
                },
                "spec": {
                    "restartPolicy": "OnFailure",
                    "containers": [
                        {
                            "name": &CONTAINER_NAME,
                            "image": image,
                            "ports": [{ "containerPort": 80 }],
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
            },
        }
    }))
    .expect("Failed creating `job` from json spec!")
}

fn default_env() -> Value {
    json!(
        [
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
    )
}

/// Create a new [`KubeService`] and related Kubernetes resources. The resources will be deleted
/// when the returned service is dropped, unless it is dropped during panic.
/// This behavior can be changed, see [`PRESERVE_FAILED_ENV_NAME`].
/// * `randomize_name` - whether a random suffix should be added to the end of the resource names
#[fixture]
pub async fn service(
    #[default("default")] namespace: &str,
    #[default("NodePort")] service_type: &str,
    #[default("ghcr.io/metalbear-co/mirrord-pytest:latest")] image: &str,
    #[default("http-echo")] service_name: &str,
    #[default(true)] randomize_name: bool,
    #[future] kube_client: Client,
) -> KubeService {
    internal_service(
        namespace,
        service_type,
        image,
        service_name,
        randomize_name,
        kube_client.await,
        default_env(),
        false,
    )
    .await
}

/// Create a new [`KubeService`] and related Kubernetes resources.
///
/// The resources will be deleted
/// when the returned service is dropped, unless it is dropped during panic.
/// This behavior can be changed, see [`PRESERVE_FAILED_ENV_NAME`].
/// * `randomize_name` - whether a random suffix should be added to the end of the resource names
/// * `env` - `Value`, should be `Value::Array` of kubernetes container env var definitions.
pub async fn service_with_env(
    namespace: &str,
    service_type: &str,
    image: &str,
    service_name: &str,
    randomize_name: bool,
    kube_client: Client,
    env: Value,
) -> KubeService {
    internal_service(
        namespace,
        service_type,
        image,
        service_name,
        randomize_name,
        kube_client,
        env,
        false,
    )
    .await
}

/// Internal function to create a custom [`KubeService`].
/// We keep this private so that whenever we need more customization of test resources, we can
/// change this function and how the public ones use it, and add a new public function that exposes
/// more customization, and we don't need to change all existing usages of public functions/fixtures
/// in tests.
///
/// Create a new [`KubeService`] and related Kubernetes resources. The resources will be
/// deleted when the returned service is dropped, unless it is dropped during panic.
/// This behavior can be changed, see [`PRESERVE_FAILED_ENV_NAME`].
/// * `randomize_name` - whether a random suffix should be added to the end of the resource names
/// * `env` - `Value`, should be `Value::Array` of kubernetes container env var definitions.
async fn internal_service(
    namespace: &str,
    service_type: &str,
    image: &str,
    service_name: &str,
    randomize_name: bool,
    kube_client: Client,
    env: Value,
    ipv6_only: bool,
) -> KubeService {
    let delete_after_fail = std::env::var_os(PRESERVE_FAILED_ENV_NAME).is_none();

    let namespace_api: Api<Namespace> = Api::all(kube_client.clone());
    let deployment_api: Api<Deployment> = Api::namespaced(kube_client.clone(), namespace);
    let service_api: Api<Service> = Api::namespaced(kube_client.clone(), namespace);

    let name = if randomize_name {
        format!("{}-{}", service_name, random_string())
    } else {
        // If using a non-random name, delete existing resources first.
        // Just continue if they don't exist.
        // Force delete
        let delete_params = DeleteParams {
            grace_period_seconds: Some(0),
            ..Default::default()
        };

        let _ = service_api.delete(service_name, &delete_params).await;
        let _ = deployment_api.delete(service_name, &delete_params).await;

        service_name.to_string()
    };

    println!(
        "{} creating service {name:?} in namespace {namespace:?}",
        format_time()
    );

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

    // `Deployment`
    let deployment = deployment_from_json(&name, image, env);
    let pod_guard = ResourceGuard::create(
        deployment_api.clone(),
        name.to_string(),
        &deployment,
        delete_after_fail,
    )
    .await
    .unwrap();
    watch_resource_exists(&deployment_api, &name).await;

    // `Service`
    let mut service = service_from_json(&name, service_type);
    if ipv6_only {
        set_ipv6_only(&mut service);
    }
    let service_guard = ResourceGuard::create(
        service_api.clone(),
        name.clone(),
        &service,
        delete_after_fail,
    )
    .await
    .unwrap();
    watch_resource_exists(&service_api, "default").await;

    let target = get_instance_name::<Pod>(kube_client.clone(), &name, namespace)
        .await
        .unwrap();

    let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), namespace);

    await_condition(pod_api, &target, is_pod_running())
        .await
        .unwrap();

    println!(
        "{:?} done creating service {name:?} in namespace {namespace:?}",
        Utc::now()
    );

    KubeService {
        name,
        namespace: namespace.to_string(),
        target: format!("pod/{target}/container/{CONTAINER_NAME}"),
        guards: vec![pod_guard, service_guard],
        namespace_guard,
    }
}

#[cfg(not(feature = "operator"))]
#[fixture]
pub async fn service_for_mirrord_ls(
    #[default("default")] namespace: &str,
    #[default("NodePort")] service_type: &str,
    #[default("ghcr.io/metalbear-co/mirrord-pytest:latest")] image: &str,
    #[default("http-echo")] service_name: &str,
    #[default(true)] randomize_name: bool,
    #[future] kube_client: Client,
) -> KubeService {
    let delete_after_fail = std::env::var_os(PRESERVE_FAILED_ENV_NAME).is_none();

    let kube_client = kube_client.await;
    let namespace_api: Api<Namespace> = Api::all(kube_client.clone());
    let deployment_api: Api<Deployment> = Api::namespaced(kube_client.clone(), namespace);
    let service_api: Api<Service> = Api::namespaced(kube_client.clone(), namespace);

    let name = if randomize_name {
        format!("{}-{}", service_name, random_string())
    } else {
        // If using a non-random name, delete existing resources first.
        // Just continue if they don't exist.
        // Force delete
        let delete_params = DeleteParams {
            grace_period_seconds: Some(0),
            ..Default::default()
        };

        let _ = service_api.delete(service_name, &delete_params).await;
        let _ = deployment_api.delete(service_name, &delete_params).await;

        service_name.to_string()
    };

    println!(
        "{} creating service {name:?} in namespace {namespace:?}",
        format_time()
    );

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

    // `Deployment`
    let deployment = deployment_from_json(&name, image, default_env());
    let pod_guard = ResourceGuard::create(
        deployment_api.clone(),
        name.to_string(),
        &deployment,
        delete_after_fail,
    )
    .await
    .unwrap();
    watch_resource_exists(&deployment_api, &name).await;

    // `Service`
    let service = service_from_json(&name, service_type);
    let service_guard = ResourceGuard::create(
        service_api.clone(),
        name.clone(),
        &service,
        delete_after_fail,
    )
    .await
    .unwrap();
    watch_resource_exists(&service_api, "default").await;

    let target = get_instance_name::<Pod>(kube_client.clone(), &name, namespace)
        .await
        .unwrap();

    let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), namespace);

    await_condition(pod_api, &target, is_pod_running())
        .await
        .unwrap();

    println!(
        "{:?} done creating service {name:?} in namespace {namespace:?}",
        Utc::now()
    );

    KubeService {
        name,
        namespace: namespace.to_string(),
        target: format!("pod/{target}/container/{CONTAINER_NAME}"),
        guards: vec![pod_guard, service_guard],
        namespace_guard,
    }
}

#[cfg(feature = "operator")]
#[fixture]
pub async fn service_for_mirrord_ls(
    #[default("default")] namespace: &str,
    #[default("NodePort")] service_type: &str,
    #[default("ghcr.io/metalbear-co/mirrord-pytest:latest")] image: &str,
    #[default("http-echo")] service_name: &str,
    #[default(true)] randomize_name: bool,
    #[future] kube_client: Client,
) -> KubeService {
    use k8s_openapi::api::{
        apps::v1::StatefulSet,
        batch::v1::{CronJob, Job},
    };

    let delete_after_fail = std::env::var_os(PRESERVE_FAILED_ENV_NAME).is_none();

    let kube_client = kube_client.await;
    let namespace_api: Api<Namespace> = Api::all(kube_client.clone());
    let deployment_api: Api<Deployment> = Api::namespaced(kube_client.clone(), namespace);
    let stateful_set_api: Api<StatefulSet> = Api::namespaced(kube_client.clone(), namespace);
    let cron_job_api: Api<CronJob> = Api::namespaced(kube_client.clone(), namespace);
    let job_api: Api<Job> = Api::namespaced(kube_client.clone(), namespace);
    let service_api: Api<Service> = Api::namespaced(kube_client.clone(), namespace);

    let name = if randomize_name {
        format!("{}-{}", service_name, random_string())
    } else {
        // If using a non-random name, delete existing resources first.
        // Just continue if they don't exist.
        // Force delete
        let delete_params = DeleteParams {
            grace_period_seconds: Some(0),
            ..Default::default()
        };

        let _ = service_api.delete(service_name, &delete_params).await;
        let _ = deployment_api.delete(service_name, &delete_params).await;
        let _ = stateful_set_api.delete(service_name, &delete_params).await;
        let _ = cron_job_api.delete(service_name, &delete_params).await;
        let _ = job_api.delete(service_name, &delete_params).await;

        service_name.to_string()
    };

    println!(
        "{} creating service {name:?} in namespace {namespace:?}",
        format_time()
    );

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

    // `Deployment`
    let deployment = deployment_from_json(&name, image, default_env());
    let pod_guard = ResourceGuard::create(
        deployment_api.clone(),
        name.to_string(),
        &deployment,
        delete_after_fail,
    )
    .await
    .unwrap();
    watch_resource_exists(&deployment_api, &name).await;

    // `Service`
    let service = service_from_json(&name, service_type);
    let service_guard = ResourceGuard::create(
        service_api.clone(),
        name.clone(),
        &service,
        delete_after_fail,
    )
    .await
    .unwrap();
    watch_resource_exists(&service_api, "default").await;

    // `StatefulSet`
    let stateful_set = stateful_set_from_json(&name, image);
    let stateful_set_guard = ResourceGuard::create(
        stateful_set_api.clone(),
        name.to_string(),
        &stateful_set,
        delete_after_fail,
    )
    .await
    .unwrap();
    watch_resource_exists(&stateful_set_api, &name).await;

    // `CronJob`
    let cron_job = cron_job_from_json(&name, image);
    let cron_job_guard = ResourceGuard::create(
        cron_job_api.clone(),
        name.to_string(),
        &cron_job,
        delete_after_fail,
    )
    .await
    .unwrap();
    watch_resource_exists(&cron_job_api, &name).await;

    // `Job`
    let job = job_from_json(&name, image);
    let job_guard =
        ResourceGuard::create(job_api.clone(), name.to_string(), &job, delete_after_fail)
            .await
            .unwrap();
    watch_resource_exists(&job_api, &name).await;

    let target = get_instance_name::<Pod>(kube_client.clone(), &name, namespace)
        .await
        .unwrap();

    let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), namespace);

    await_condition(pod_api, &target, is_pod_running())
        .await
        .unwrap();

    println!(
        "{:?} done creating service {name:?} in namespace {namespace:?}",
        Utc::now()
    );

    KubeService {
        name,
        namespace: namespace.to_string(),
        target: format!("pod/{target}/container/{CONTAINER_NAME}"),
        guards: vec![
            pod_guard,
            service_guard,
            stateful_set_guard,
            cron_job_guard,
            job_guard,
        ],
        namespace_guard,
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

/// Service that listens on port 80 and returns `remote: <DATA>` when getting `<DATA>` directly
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
/// that listens on port 80 and returns `remote: <DATA>` when getting `<DATA>` over a websocket
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

/// Service that listens on port 80 and returns `remote: <DATA>` when getting `<DATA>` directly
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
pub async fn random_namespace_self_deleting_service(#[future] kube_client: Client) -> KubeService {
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

/// Create a new [`KubeService`] and related Kubernetes resources. The resources will be deleted
/// when the returned service is dropped, unless it is dropped during panic.
/// This behavior can be changed, see [`PRESERVE_FAILED_ENV_NAME`].
/// * `randomize_name` - whether a random suffix should be added to the end of the resource names
#[fixture]
pub async fn ipv6_service(
    #[default("default")] namespace: &str,
    #[default("NodePort")] service_type: &str,
    // #[default("ghcr.io/metalbear-co/mirrord-pytest:latest")] image: &str,
    #[default("docker.io/t4lz/mirrord-pytest:1204")] image: &str,
    #[default("http-echo")] service_name: &str,
    #[default(true)] randomize_name: bool,
    #[future] kube_client: Client,
) -> KubeService {
    internal_service(
        namespace,
        service_type,
        image,
        service_name,
        randomize_name,
        kube_client.await,
        serde_json::json!([
            {
              "name": "HOST",
              "value": "::"
            }
        ]),
        true,
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
    get_pod_or_node_host_and_port(kube_client, &service.name, &service.namespace).await
}

async fn get_pod_or_node_host(kube_client: Client, name: &str, namespace: &str) -> String {
    let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), namespace);
    let pods = pod_api
        .list(&ListParams::default().labels(&format!("app={}", name)))
        .await
        .unwrap();
    pods.into_iter()
        .next()
        .and_then(|pod| pod.status)
        .and_then(|status| status.host_ip)
        .filter(|ip| {
            // use this IP only if it's a public one.
            match ip.parse::<IpAddr>().unwrap() {
                IpAddr::V4(ip4) => ip4.is_private(),
                IpAddr::V6(ip6) => {
                    ip6.is_unicast_link_local() || ip6.is_unique_local() || ip6.is_loopback()
                }
            }
            .not()
        })
        .unwrap_or_else(resolve_node_host)
}

async fn get_pod_or_node_host_and_port(
    kube_client: Client,
    name: &str,
    namespace: &str,
) -> (String, i32) {
    let host_ip = get_pod_or_node_host(kube_client.clone(), name, namespace).await;
    let services_api: Api<Service> = Api::namespaced(kube_client.clone(), namespace);
    let services = services_api
        .list(&ListParams::default().labels(&format!("app={}", name)))
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

/// Returns a name of any pod/deployment belonging to the given app.
pub async fn get_instance_name<T>(client: Client, app_name: &str, namespace: &str) -> Option<String>
where
    T: kube::Resource<Scope = k8s_openapi::NamespaceResourceScope>
        + k8s_openapi::Metadata
        + Clone
        + DeserializeOwned
        + Debug,
    <T as kube::Resource>::DynamicType: Default,
{
    let api: Api<T> = Api::namespaced(client, namespace);
    api.list(&ListParams::default().labels(&format!("app={}", app_name)))
        .await
        .unwrap()
        .into_iter()
        .find_map(|item| item.meta().name.clone())
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

pub async fn send_requests(url: &str, expect_response: bool, headers: reqwest::header::HeaderMap) {
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

#[fixture]
#[once]
pub fn config_dir() -> PathBuf {
    let mut config_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    config_path.push("configs");
    config_path
}
