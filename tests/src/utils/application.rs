use super::{run_exec_targetless, run_exec_with_target, TestProcess};

pub(crate) mod env;
pub(crate) mod file_ops;

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
