#![allow(clippy::unused_io_amount)]
#![allow(clippy::indexing_slicing)]

use std::{
    collections::HashMap,
    fmt::Debug,
    ops::Not,
    os::unix::process::ExitStatusExt,
    path::PathBuf,
    process::{ExitStatus, Stdio},
    sync::{Arc, Once},
    time::Duration,
};

use chrono::{Timelike, Utc};
use fancy_regex::Regex;
use futures::FutureExt;
use futures_util::future::BoxFuture;
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, Service},
};
use kube::{
    api::{DeleteParams, PostParams},
    Api, Client, Config, Error, Resource,
};
use rand::distr::{Alphanumeric, SampleString};
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

pub(crate) mod ipv6;
pub(crate) mod port_forwarder;
pub mod sqs_resources;
pub(crate) mod watch;

const TEXT: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.";
pub const CONTAINER_NAME: &str = "test";

/// Name of the environment variable used to control cleanup after failed tests.
/// By default, resources from failed tests are deleted.
/// However, if this variable is set, resources will always be preserved.
pub const PRESERVE_FAILED_ENV_NAME: &str = "MIRRORD_E2E_PRESERVE_FAILED";

/// All Kubernetes resources created for testing purposes share this label.
pub const TEST_RESOURCE_LABEL: (&str, &str) = ("mirrord-e2e-test-resource", "true");

/// Creates a random string of 7 alphanumeric lowercase characters.
pub(crate) fn random_string() -> String {
    Alphanumeric
        .sample_string(&mut rand::rng(), 7)
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
    /// Can run as both HTTP and HTTPS server, listens on port 80.
    ///
    /// Env:
    /// * `SERVER_TLS_CERT`: path to a PEM file with certificate chain to use
    /// * `SERVER_TLS_KEY`: path to a PEM file with a private key matching the certificate
    ///
    /// Pass both `SERVER_CERT` and `SERVER_KEY` to enable HTTPS mode.
    Go21HTTP,
    /// Can run as both HTTP and HTTPS server, listens on port 80.
    ///
    /// Env:
    /// * `SERVER_TLS_CERT`: path to a PEM file with certificate chain to use
    /// * `SERVER_TLS_KEY`: path to a PEM file with a private key matching the certificate
    ///
    /// Pass both `SERVER_CERT` and `SERVER_KEY` to enable HTTPS mode.
    Go22HTTP,
    /// Can run as both HTTP and HTTPS server, listens on port 80.
    ///
    /// Env:
    /// * `SERVER_TLS_CERT`: path to a PEM file with certificate chain to use
    /// * `SERVER_TLS_KEY`: path to a PEM file with a private key matching the certificate
    ///
    /// Pass both `SERVER_CERT` and `SERVER_KEY` to enable HTTPS mode.
    Go23HTTP,
    CurlToKubeApi,
    CurlToKubeApiOverIpv6,
    PythonCloseSocket,
    PythonCloseSocketKeepConnection,
    RustWebsockets,
    RustSqs,
    /// Tries to open files in the remote target, but these operations should succeed or fail based
    /// on mirrord `FsPolicy`.
    ///
    /// - `node-e2e/fspolicy/test_operator_fs_policy.mjs`
    NodeFsPolicy,
}

#[derive(Debug)]
pub enum FileOps {
    Python,
    Rust,
    GoDir21,
    GoDir22,
    GoDir23,
    GoStatfs,
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
            assert!(
                self.stderr_data.read().await.contains(level).not(),
                "application stderr should not contain `{level}`"
            );
        } else {
            assert!(
                self.stdout_data.read().await.contains(level).not(),
                "application stdout should not contain `{level}`"
            );
        }
    }

    pub async fn assert_python_fileops_stderr(&self) {
        assert!(
            self.stderr_data.read().await.contains("FAILED").not(),
            "application stderr should not contain `FAILED`"
        );
    }

    pub async fn wait_assert_success(&mut self) {
        let output = self.wait().await;
        assert!(
            output.success(),
            "application unexpectedly failed: exit code {:?}, signal code {:?}",
            output.code(),
            output.signal(),
        );
    }

    pub async fn wait_assert_fail(&mut self) {
        let output = self.wait().await;
        assert!(
            output.success().not(),
            "application unexpectedly succeeded: exit code {:?}, signal code {:?}",
            output.code(),
            output.signal()
        );
    }

    pub async fn assert_stdout_contains(&self, string: &str) {
        assert!(
            self.get_stdout().await.contains(string),
            "application stdout should contain `{string}`",
        );
    }

    pub async fn assert_stdout_doesnt_contain(&self, string: &str) {
        assert!(
            self.get_stdout().await.contains(string).not(),
            "application stdout should not contain `{string}`",
        );
    }

    pub async fn assert_stderr_contains(&self, string: &str) {
        assert!(
            self.get_stderr().await.contains(string),
            "application stderr should contain `{string}`",
        );
    }

    pub async fn assert_stderr_doesnt_contain(&self, string: &str) {
        assert!(
            self.get_stderr().await.contains(string).not(),
            "application stderr should not contain `{string}`",
        );
    }

    pub async fn assert_no_error_in_stdout(&self) {
        assert!(
            self.error_capture
                .is_match(&self.stdout_data.read().await)
                .unwrap()
                .not(),
            "application stdout contains an error"
        );
    }

    pub async fn assert_no_error_in_stderr(&self) {
        assert!(
            self.error_capture
                .is_match(&self.stderr_data.read().await)
                .unwrap()
                .not(),
            "application stderr contains an error"
        );
    }

    pub async fn assert_no_warn_in_stdout(&self) {
        assert!(
            self.warn_capture
                .is_match(&self.stdout_data.read().await)
                .unwrap()
                .not(),
            "application stdout contains a warning"
        );
    }

    pub async fn assert_no_warn_in_stderr(&self) {
        assert!(
            self.warn_capture
                .is_match(&self.stderr_data.read().await)
                .unwrap()
                .not(),
            "application stderr contains a warning"
        );
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

    pub async fn wait_for_line_stdout(&self, timeout: Duration, line: &str) {
        let now = std::time::Instant::now();
        while now.elapsed() < timeout {
            let stdout = self.get_stdout().await;
            if stdout.contains(line) {
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
        env: HashMap<String, String>,
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
            Application::NodeHTTP => vec!["node", "node-e2e/app.mjs"],
            Application::NodeHTTP2 => {
                vec!["node", "node-e2e/http2/test_http2_traffic_steal.mjs"]
            }
            Application::NodeFsPolicy => {
                vec!["node", "node-e2e/fspolicy/test_operator_fs_policy.mjs"]
            }
            Application::Go21HTTP => vec!["go-e2e/21.go_test_app"],
            Application::Go22HTTP => vec!["go-e2e/22.go_test_app"],
            Application::Go23HTTP => vec!["go-e2e/23.go_test_app"],
            Application::CurlToKubeApi => {
                vec!["curl", "https://kubernetes/api", "--insecure"]
            }
            Application::CurlToKubeApiOverIpv6 => {
                vec!["curl", "-6", "https://kubernetes/api", "--insecure"]
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
            FileOps::Python => {
                vec!["python3", "-B", "-m", "unittest", "-f", "python-e2e/ops.py"]
            }
            FileOps::Rust => vec!["../target/debug/rust-e2e-fileops"],
            FileOps::GoDir21 => vec!["go-e2e-dir/21.go_test_app"],
            FileOps::GoDir22 => vec!["go-e2e-dir/22.go_test_app"],
            FileOps::GoDir23 => vec!["go-e2e-dir/23.go_test_app"],
            FileOps::GoStatfs => vec!["go-e2e-statfs/23.go_test_app"],
        }
    }

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
    let agent_image_env = "MIRRORD_AGENT_IMAGE";
    let agent_image_from_devs_env = std::env::var(agent_image_env);
    // used by the CI, to load the image locally:
    // docker build -t test . -f mirrord/agent/Dockerfile
    // minikube load image test:latest
    let mut base_env = HashMap::new();
    base_env.insert(
        agent_image_env,
        // Let devs running the test specify an agent image per env var.
        agent_image_from_devs_env.as_deref().unwrap_or("test"),
    );
    base_env.insert("MIRRORD_CHECK_VERSION", "false");
    base_env.insert("MIRRORD_AGENT_RUST_LOG", "warn,mirrord=debug");
    base_env.insert("MIRRORD_AGENT_COMMUNICATION_TIMEOUT", "180");
    // We're using k8s portforwarding for sending traffic to test services.
    // The packets arrive to loopback interface.
    base_env.insert("MIRRORD_AGENT_NETWORK_INTERFACE", "lo");
    base_env.insert("RUST_LOG", "warn,mirrord=debug");

    if let Some(env) = env {
        for (key, value) in env {
            base_env.insert(key, value);
        }
    }

    run_mirrord(args, base_env).await
}

/// Runs `mirrord ls` command.
pub async fn run_ls(namespace: &str) -> TestProcess {
    let mut mirrord_args = vec!["ls"];
    mirrord_args.extend(vec!["--namespace", namespace]);

    let mut env = HashMap::new();
    let use_operator = cfg!(feature = "operator").to_string();
    env.insert("MIRRORD_OPERATOR_ENABLE", use_operator.as_str());

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
    pub async fn create<
        K: Resource<DynamicType = ()> + Debug + Clone + DeserializeOwned + Serialize + 'static,
    >(
        api: Api<K>,
        data: &K,
        delete_on_fail: bool,
    ) -> Result<(ResourceGuard, K), Error> {
        let name = data.meta().name.clone().unwrap();
        println!("Creating {} `{name}`: {data:?}", K::kind(&()));
        let created = api.create(&PostParams::default(), data).await?;
        println!("Created {} `{name}`", K::kind(&()));

        let deleter = async move {
            println!("Deleting {} `{name}`", K::kind(&()));
            let delete_params = DeleteParams {
                grace_period_seconds: Some(0),
                ..Default::default()
            };
            let res = api.delete(&name, &delete_params).await;
            if let Err(e) = res {
                println!("Failed to delete {} `{name}`: {e:?}", K::kind(&()));
            }
        };

        Ok((
            Self {
                delete_on_fail,
                deleter: Some(deleter.boxed()),
            },
            created,
        ))
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
    pub service: Service,
    pub deployment: Deployment,
    guards: Vec<ResourceGuard>,
    namespace_guard: Option<ResourceGuard>,
    pub pod_name: String,
}

impl KubeService {
    pub fn deployment_target(&self) -> String {
        format!("deployment/{}", self.name)
    }

    pub fn pod_container_target(&self) -> String {
        format!("pod/{}/container/{CONTAINER_NAME}", self.pod_name)
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
                    "name": "http",
                    "protocol": "TCP",
                    "port": 80,
                    "targetPort": 80,
                },
                {
                    "name": "udp",
                    "protocol": "UDP",
                    "port": 31415,
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
#[allow(clippy::too_many_arguments)]
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
        "{} creating service {name} in namespace {namespace}",
        format_time()
    );

    // Create namespace and wrap it in ResourceGuard if it does not yet exist.
    let namespace_guard = ResourceGuard::create::<Namespace>(
        namespace_api.clone(),
        &serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Namespace",
            "metadata": {
                "name": namespace,
                "labels": {
                    TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
                }
            },
        }))
        .unwrap(),
        delete_after_fail,
    )
    .await
    .ok();

    // `Deployment`
    let deployment = deployment_from_json(&name, image, env);
    let (deployment_guard, deployment) =
        ResourceGuard::create(deployment_api.clone(), &deployment, delete_after_fail)
            .await
            .unwrap();

    // `Service`
    let mut service = service_from_json(&name, service_type);
    if ipv6_only {
        set_ipv6_only(&mut service);
    }
    let (service_guard, service) =
        ResourceGuard::create(service_api.clone(), &service, delete_after_fail)
            .await
            .unwrap();

    let ready_pod = watch::wait_until_pods_ready(&service, 1, kube_client.clone())
        .await
        .into_iter()
        .next()
        .unwrap();
    let pod_name = ready_pod.metadata.name.unwrap();

    println!(
        "{} done creating service {name} in namespace {namespace}",
        format_time(),
    );

    KubeService {
        name,
        namespace: namespace.to_string(),
        service,
        deployment,
        pod_name,
        guards: vec![deployment_guard, service_guard],
        namespace_guard: namespace_guard.map(|(guard, _)| guard),
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
        &namespace_resource,
        delete_after_fail,
    )
    .await
    .ok();

    // `Deployment`
    let deployment = deployment_from_json(&name, image, default_env());
    let (deployment_guard, deployment) =
        ResourceGuard::create(deployment_api.clone(), &deployment, delete_after_fail)
            .await
            .unwrap();

    // `Service`
    let service = service_from_json(&name, service_type);
    let (service_guard, service) =
        ResourceGuard::create(service_api.clone(), &service, delete_after_fail)
            .await
            .unwrap();

    let pod_name = watch::wait_until_pods_ready(&service, 1, kube_client)
        .await
        .into_iter()
        .next()
        .unwrap()
        .metadata
        .name
        .unwrap();

    println!(
        "{:?} done creating service {name:?} in namespace {namespace:?}",
        Utc::now()
    );

    KubeService {
        name,
        namespace: namespace.to_string(),
        service,
        deployment,
        pod_name,
        guards: vec![deployment_guard, service_guard],
        namespace_guard: namespace_guard.map(|(guard, _)| guard),
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
        &namespace_resource,
        delete_after_fail,
    )
    .await
    .ok();

    // `Deployment`
    let deployment = deployment_from_json(&name, image, default_env());
    let (deployment_guard, deployment) =
        ResourceGuard::create(deployment_api.clone(), &deployment, delete_after_fail)
            .await
            .unwrap();

    // `Service`
    let service = service_from_json(&name, service_type);
    let (service_guard, service) =
        ResourceGuard::create(service_api.clone(), &service, delete_after_fail)
            .await
            .unwrap();

    // `StatefulSet`
    let stateful_set = stateful_set_from_json(&name, image);
    let (stateful_set_guard, _) =
        ResourceGuard::create(stateful_set_api.clone(), &stateful_set, delete_after_fail)
            .await
            .unwrap();

    // `CronJob`
    let cron_job = cron_job_from_json(&name, image);
    let (cron_job_guard, _) =
        ResourceGuard::create(cron_job_api.clone(), &cron_job, delete_after_fail)
            .await
            .unwrap();

    // `Job`
    let job = job_from_json(&name, image);
    let (job_guard, _) = ResourceGuard::create(job_api.clone(), &job, delete_after_fail)
        .await
        .unwrap();

    let pod_name = watch::wait_until_pods_ready(&service, 1, kube_client.clone())
        .await
        .into_iter()
        .next()
        .unwrap()
        .metadata
        .name
        .unwrap();

    println!(
        "{:?} done creating service {name} in namespace {namespace}",
        Utc::now()
    );

    KubeService {
        name,
        namespace: namespace.to_string(),
        service,
        deployment,
        guards: vec![
            deployment_guard,
            service_guard,
            stateful_set_guard,
            cron_job_guard,
            job_guard,
        ],
        namespace_guard: namespace_guard.map(|(guard, _)| guard),
        pod_name,
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

#[fixture]
pub async fn go_statfs_service(#[future] kube_client: Client) -> KubeService {
    service(
        "default",
        "ClusterIP",
        "ghcr.io/metalbear-co/mirrord-go-statfs:latest",
        "go-statfs",
        true,
        kube_client,
    )
    .await
}

/// Take a request builder of any method, add headers, send the request, verify success, and
/// optionally verify expected response.
pub async fn send_request(
    request_builder: RequestBuilder,
    expect_response: Option<&str>,
    headers: reqwest::header::HeaderMap,
) {
    let (client, request) = request_builder.headers(headers).build_split();
    let request = request.unwrap();
    println!(
        "Sending an HTTP request with version={:?}, method=({}), url=({}), headers=({:?})",
        request.version(),
        request.method(),
        request.url(),
        request.headers(),
    );

    let response = client.execute(request).await.unwrap();

    let status = response.status();
    let body = String::from_utf8_lossy(response.bytes().await.unwrap().as_ref()).into_owned();

    assert_eq!(
        status,
        StatusCode::OK,
        "unexpected status, response body: {body}"
    );

    if let Some(expected_response) = expect_response {
        assert_eq!(body, expected_response);
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
