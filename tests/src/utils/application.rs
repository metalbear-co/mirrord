#![allow(dead_code)]
use std::{fmt, time::Duration};

use mirrord_test_utils::{
    TestProcess, node, python,
    run_command::{run_exec_targetless, run_exec_with_target},
};

pub mod env;
pub(crate) mod file_ops;

/// Go versions used with test applications.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy)]
pub enum GoVersion {
    GO_1_25,
    GO_1_26,
    GO_1_27,
}

impl GoVersion {
    pub const LATEST: Self = Self::GO_1_27;
}

impl fmt::Display for GoVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::GO_1_25 => "25",
            Self::GO_1_26 => "26",
            Self::GO_1_27 => "27",
        };
        f.write_str(as_str)
    }
}

#[derive(Debug)]
pub enum Application {
    PythonFlaskHTTP,
    PythonFastApiHTTP,
    PythonFastApiHTTPIPv6,
    NodeHTTP,
    NodeHTTP2,
    CurlToKubeApi,
    CurlToKubeApiOverIpv6,
    PythonCloseSocket,
    PythonCloseSocketKeepConnection,
    RustWebsockets,
    /// Tries to open files in the remote target, but these operations should succeed or fail based
    /// on mirrord `FsPolicy`.
    ///
    /// - `node-e2e/fspolicy/test-operator-fs-policy/main.mjs`
    NodeFsPolicy,
    /// Waits for any child process to finish, and verifies that the `wait` call fails with
    /// `ECHILD`.
    IntproxyChild,
}

impl Application {
    /// Returns the command that runs the app.
    ///
    /// The paths of the apps are relative to the `tests` directory, because nextest runs
    /// each test in this package's directory in the test's runfiles, where Bazel puts the apps.
    /// They stay relative, rather than start from the runfiles, because the layer leaves a relative
    /// path local, and the apps' interpreters open the scripts by these paths.
    pub fn get_cmd(&self) -> Vec<String> {
        match self {
            Application::PythonFlaskHTTP => {
                vec![
                    python(),
                    "-u".into(),
                    "apps/python-e2e/app-flask/main.py".to_owned(),
                ]
            }
            Application::PythonFastApiHTTP => vec![
                python(),
                "-m".into(),
                "uvicorn".into(),
                "--port=80".into(),
                "--host=0.0.0.0".into(),
                "--app-dir=./apps/python-e2e/app-fastapi/".to_owned(),
                "main:app".into(),
            ],
            Application::PythonFastApiHTTPIPv6 => vec![
                python(),
                "-m".into(),
                "uvicorn".into(),
                "--port=80".into(),
                "--host=::".into(),
                "--app-dir=./apps/python-e2e/app-fastapi/".to_owned(),
                "main:app".into(),
            ],
            Application::PythonCloseSocket => {
                vec![
                    python(),
                    "-u".into(),
                    "apps/python-e2e/close-socket/main.py".to_owned(),
                ]
            }
            Application::PythonCloseSocketKeepConnection => vec![
                python(),
                "-u".into(),
                "apps/python-e2e/close-socket-keep-connection/main.py".to_owned(),
            ],
            Application::NodeHTTP => vec![node(), "apps/node-e2e/app/main.mjs".to_owned()],
            Application::NodeHTTP2 => {
                vec![
                    node(),
                    "apps/node-e2e/http2/test-http2-traffic-steal/main.mjs".to_owned(),
                ]
            }
            Application::NodeFsPolicy => {
                vec![
                    node(),
                    "apps/node-e2e/fspolicy/test-operator-fs-policy/main.mjs".to_owned(),
                ]
            }
            Application::CurlToKubeApi => ["curl", "https://kubernetes/api", "--insecure"]
                .map(String::from)
                .to_vec(),
            Application::CurlToKubeApiOverIpv6 => {
                ["curl", "-6", "https://kubernetes/api", "--insecure"]
                    .map(String::from)
                    .to_vec()
            }
            Application::RustWebsockets => vec!["apps/rust-websockets/rust-websockets".to_owned()],
            Application::IntproxyChild => vec!["apps/intproxy-child/intproxy-child".to_owned()],
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

    pub async fn wait_until_listening(&self, process: &TestProcess) {
        // Wait for application-specific startup message first
        match self {
            Application::PythonFastApiHTTP => {
                process
                    .wait_for_line(Duration::from_secs(120), "Application startup complete")
                    .await;
            }
            Application::PythonFlaskHTTP => {
                process
                    .wait_for_line_stdout(Duration::from_secs(120), "Server listening on port 80")
                    .await;
            }
            _ => {}
        };

        process
            .wait_for_line(Duration::from_secs(60), "daemon subscribed")
            .await;
    }
}
