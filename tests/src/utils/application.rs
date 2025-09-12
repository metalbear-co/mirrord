#![allow(dead_code)]
use std::fmt;

use super::TestProcess;
use crate::utils::run_command::{run_exec_targetless, run_exec_with_target};

pub(crate) mod env;
pub(crate) mod file_ops;

/// Go versions used with test applications.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy)]
pub enum GoVersion {
    GO_1_23,
    GO_1_24,
    GO_1_25,
}

impl fmt::Display for GoVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::GO_1_23 => "23",
            Self::GO_1_24 => "24",
            Self::GO_1_25 => "25",
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

impl Application {
    pub fn get_cmd(&self) -> Vec<String> {
        match self {
            Application::PythonFlaskHTTP => ["python3", "-u", "python-e2e/app_flask.py"]
                .map(String::from)
                .to_vec(),
            Application::PythonFastApiHTTP => [
                "uvicorn",
                "--port=80",
                "--host=0.0.0.0",
                "--app-dir=./python-e2e/",
                "app_fastapi:app",
            ]
            .map(String::from)
            .to_vec(),
            Application::PythonFastApiHTTPIPv6 => [
                "uvicorn",
                "--port=80",
                "--host=::",
                "--app-dir=./python-e2e/",
                "app_fastapi:app",
            ]
            .map(String::from)
            .to_vec(),
            Application::PythonCloseSocket => ["python3", "-u", "python-e2e/close_socket.py"]
                .map(String::from)
                .to_vec(),
            Application::PythonCloseSocketKeepConnection => [
                "python3",
                "-u",
                "python-e2e/close_socket_keep_connection.py",
            ]
            .map(String::from)
            .to_vec(),
            Application::NodeHTTP => ["node", "node-e2e/app.mjs"].map(String::from).to_vec(),
            Application::NodeHTTP2 => ["node", "node-e2e/http2/test_http2_traffic_steal.mjs"]
                .map(String::from)
                .to_vec(),
            Application::NodeFsPolicy => ["node", "node-e2e/fspolicy/test_operator_fs_policy.mjs"]
                .map(String::from)
                .to_vec(),
            Application::GoHTTP(go_version) => vec![format!("go-e2e/{go_version}.go_test_app")],
            Application::CurlToKubeApi => ["curl", "https://kubernetes/api", "--insecure"]
                .map(String::from)
                .to_vec(),
            Application::CurlToKubeApiOverIpv6 => {
                ["curl", "-6", "https://kubernetes/api", "--insecure"]
                    .map(String::from)
                    .to_vec()
            }
            Application::RustWebsockets => ["../target/debug/rust-websockets"]
                .map(String::from)
                .to_vec(),
            Application::RustSqs => ["../target/debug/rust-sqs-printer"]
                .map(String::from)
                .to_vec(),
            Application::IntproxyChild => {
                ["intproxy_child/out.c_test_app"].map(String::from).to_vec()
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

    pub async fn assert(&self, process: &TestProcess) {
        if matches!(self, Self::PythonFastApiHTTP | Self::PythonFastApiHTTPIPv6) {
            process.assert_log_level(true, "ERROR").await;
            process.assert_log_level(false, "ERROR").await;
            process.assert_log_level(true, "CRITICAL").await;
            process.assert_log_level(false, "CRITICAL").await;
        }
    }
}
