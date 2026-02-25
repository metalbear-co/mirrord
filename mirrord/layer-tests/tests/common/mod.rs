use std::{
    collections::HashMap,
    env,
    fmt::Debug,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use mirrord_config::MIRRORD_TEST_INTPROXY_ADDR;
use mirrord_layer_tests::intproxy::TestIntProxy;
pub use mirrord_test_utils::{TestProcess, run_command::run_exec_targetless};
use rstest::fixture;
use tokio::net::TcpListener;

/// Configuration for [`Application::RustOutgoingTcp`] and [`Application::RustOutgoingUdp`].
pub const RUST_OUTGOING_PEERS: &str = "1.1.1.1:1111,2.2.2.2:2222,3.3.3.3:3333";
/// Configuration for [`Application::RustOutgoingTcp`] and [`Application::RustOutgoingUdp`].
pub const RUST_OUTGOING_LOCAL: &str = "4.4.4.4:4444";

/// Various applications used by integration tests.
#[derive(Debug)]
pub enum Application {
    RustOutgoingUdp,
    RustOutgoingTcp { non_blocking: bool },
}

impl Application {
    pub async fn get_executable(&self) -> String {
        match self {
            Application::RustOutgoingUdp | Application::RustOutgoingTcp { .. } => format!(
                "{}/{}",
                env!("CARGO_MANIFEST_DIR"),
                "../../target/debug/outgoing",
            ),
        }
    }

    pub fn get_args(&self) -> Vec<String> {
        let mut app_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // Note: until we move layer IT tests to this crate, we piggy-back layer's apps
        app_path.push("../layer/tests/apps/");
        match self {
            Application::RustOutgoingUdp => ["--udp", RUST_OUTGOING_LOCAL, RUST_OUTGOING_PEERS]
                .into_iter()
                .map(Into::into)
                .collect(),
            Application::RustOutgoingTcp {
                non_blocking: false,
            } => ["--tcp", RUST_OUTGOING_LOCAL, RUST_OUTGOING_PEERS]
                .into_iter()
                .map(Into::into)
                .collect(),
            Application::RustOutgoingTcp { non_blocking: true } => [
                "--tcp",
                RUST_OUTGOING_LOCAL,
                RUST_OUTGOING_PEERS,
                "--non-blocking",
            ]
            .into_iter()
            .map(Into::into)
            .collect(),
        }
    }

    pub fn get_app_port(&self) -> u16 {
        match self {
            Application::RustOutgoingUdp | Application::RustOutgoingTcp { .. } => {
                unimplemented!("shouldn't get here")
            }
        }
    }

    pub async fn get_test_process(
        &self,
        env: HashMap<String, String>,
        config_path: Option<&Path>,
    ) -> TestProcess {
        let executable = self.get_executable().await;
        println!("Using executable: {}", &executable);
        println!("Using args: {:?}", self.get_args());

        let cli_args_owned: Option<Vec<String>> = config_path.map(|path| {
            vec![
                "--config-file".to_string(),
                path.to_string_lossy().to_string(),
            ]
        });
        let cli_args = cli_args_owned
            .as_ref()
            .map(|args| args.iter().map(|s| s.as_str()).collect());
        let env_pairs: Vec<(String, String)> = env.into_iter().collect();
        let env_refs: Vec<(&str, &str)> = env_pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        let cmdline: Vec<String> = [executable].into_iter().chain(self.get_args()).collect();
        eprintln!("{:?}", cmdline);
        run_exec_targetless(cmdline, None, cli_args, Some(env_refs)).await
    }

    pub async fn start_process_with_layer(
        &self,
        extra_env_vars: Vec<(&str, &str)>,
        configuration_file: Option<&Path>,
    ) -> (TestProcess, TestIntProxy) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let env = get_env(address, extra_env_vars);
        let test_process = self.get_test_process(env, configuration_file).await;

        (
            test_process,
            TestIntProxy::new(listener, configuration_file).await,
        )
    }

    /// Like `start_process_with_layer`, but also verify a port subscribe.
    pub async fn start_process_with_layer_and_port(
        &self,
        extra_env_vars: Vec<(&str, &str)>,
        configuration_file: Option<&Path>,
    ) -> (TestProcess, TestIntProxy) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let env = get_env(address, extra_env_vars);
        let test_process = self.get_test_process(env, configuration_file).await;

        (
            test_process,
            TestIntProxy::new_with_app_port(listener, self.get_app_port(), configuration_file)
                .await,
        )
    }
}

/// Fixture to get configuration files directory.
#[fixture]
#[once]
pub fn config_dir() -> PathBuf {
    let mut config_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // Note: until we move layer IT tests to this crate, we piggy-back layer's configs
    config_path.push("../layer/tests/configs");
    config_path
}

/// Environment for the user application.
///
/// The environment includes:
/// 1. `RUST_LOG=warn,mirrord=trace`
/// 2. [`MIRRORD_TEST_INTPROXY_ADDR`] (for `mirrord exec` override)
/// 3. Optional execution controls (`MIRRORD_CHECK_VERSION`, `MIRRORD_PROGRESS_MODE`)
/// 4. Given `extra_vars`
#[allow(unused_variables)]
pub fn get_env(
    intproxy_addr: SocketAddr,
    extra_vars: Vec<(&str, &str)>,
) -> HashMap<String, String> {
    let extra_vars_owned = extra_vars
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<Vec<_>>();

    [
        ("RUST_LOG".to_string(), "warn,mirrord=debug".to_string()),
        ("MIRRORD_CHECK_VERSION".to_string(), "false".to_string()),
        ("MIRRORD_PROGRESS_MODE".to_string(), "off".to_string()),
        (
            MIRRORD_TEST_INTPROXY_ADDR.to_string(),
            intproxy_addr.to_string(),
        ),
    ]
    .into_iter()
    .chain(extra_vars_owned)
    .collect()
}
