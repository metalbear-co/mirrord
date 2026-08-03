#![allow(dead_code)]
use std::{fmt, time::Duration};

use mirrord_test_utils::{
    run_command::{run_exec_targetless, run_exec_with_target},
    TestProcess,
};

pub mod env;
pub(crate) mod file_ops;

/// Go versions used with test applications.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy)]
pub enum GoVersion {
    GO_1_24,
    GO_1_25,
    GO_1_26,
}

impl GoVersion {
    pub const LATEST: Self = Self::GO_1_26;
}

impl fmt::Display for GoVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::GO_1_24 => "24",
            Self::GO_1_25 => "25",
            Self::GO_1_26 => "26",
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
    /// Can run as both HTTP and HTTPS server, listens on port 80.
    ///
    /// Env:
    /// * `SERVER_TLS_CERT`: path to a PEM file with certificate chain to use
    /// * `SERVER_TLS_KEY`: path to a PEM file with a private key matching the certificate
    ///
    /// Pass both `SERVER_CERT` and `SERVER_KEY` to enable HTTPS mode.
    GoHTTP(GoVersion),
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
    /// Waits for any child process to finish, and verifies that the `wait` call fails with
    /// `ECHILD`.
    IntproxyChild,
}

/// Resolves an app path (relative to the `tests` directory) against the configured app root.
///
/// The test apps are referenced by paths relative to the `tests` directory, which works when the
/// suite runs from this repo. `MIRRORD_TESTS_APP_ROOT` points at that directory when it is
/// elsewhere: the operator suite sets it to the mirrord checkout staged in the E2E runner image, so
/// these apps resolve there instead of in the operator's own checkout. Unset, paths are returned
/// unchanged.
pub(crate) fn app_path(relative: &str) -> String {
    match std::env::var("MIRRORD_TESTS_APP_ROOT") {
        Ok(root) if !root.is_empty() => {
            format!(
                "{}/{}",
                root.trim_end_matches('/'),
                relative.trim_start_matches("./")
            )
        }
        _ => relative.to_owned(),
    }
}

impl Application {
    pub fn get_cmd(&self) -> Vec<String> {
        match self {
            Application::PythonFlaskHTTP => {
                vec![
                    "python3".into(),
                    "-u".into(),
                    app_path("python-e2e/app_flask.py"),
                ]
            }
            Application::PythonFastApiHTTP => vec![
                "uvicorn".into(),
                "--port=80".into(),
                "--host=0.0.0.0".into(),
                format!("--app-dir={}", app_path("./python-e2e/")),
                "app_fastapi:app".into(),
            ],
            Application::PythonFastApiHTTPIPv6 => vec![
                "uvicorn".into(),
                "--port=80".into(),
                "--host=::".into(),
                format!("--app-dir={}", app_path("./python-e2e/")),
                "app_fastapi:app".into(),
            ],
            Application::PythonCloseSocket => {
                vec![
                    "python3".into(),
                    "-u".into(),
                    app_path("python-e2e/close_socket.py"),
                ]
            }
            Application::PythonCloseSocketKeepConnection => vec![
                "python3".into(),
                "-u".into(),
                app_path("python-e2e/close_socket_keep_connection.py"),
            ],
            Application::NodeHTTP => vec!["node".into(), app_path("node-e2e/app.mjs")],
            Application::NodeHTTP2 => {
                vec![
                    "node".into(),
                    app_path("node-e2e/http2/test_http2_traffic_steal.mjs"),
                ]
            }
            Application::NodeFsPolicy => {
                vec![
                    "node".into(),
                    app_path("node-e2e/fspolicy/test_operator_fs_policy.mjs"),
                ]
            }
            Application::GoHTTP(go_version) => {
                vec![app_path(&format!("go-e2e/{go_version}.go_test_app"))]
            }
            Application::CurlToKubeApi => ["curl", "https://kubernetes/api", "--insecure"]
                .map(String::from)
                .to_vec(),
            Application::CurlToKubeApiOverIpv6 => {
                ["curl", "-6", "https://kubernetes/api", "--insecure"]
                    .map(String::from)
                    .to_vec()
            }
            Application::RustWebsockets => vec![app_path("../target/debug/rust-websockets")],
            Application::RustSqs => vec![app_path("../target/debug/rust-sqs-printer")],
            Application::IntproxyChild => vec![app_path("intproxy_child/out.c_test_app")],
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
