#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::sync::OnceLock;
use std::{
    collections::HashMap,
    fmt::{self, Debug},
    fs::File,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};

use mirrord_config::{LayerConfig, MIRRORD_LAYER_INTPROXY_ADDR, config::ConfigContext};
pub use mirrord_layer_tests::intproxy::TestIntProxy;
#[cfg(target_os = "macos")]
use mirrord_sip::{SipPatchOptions, sip_patch};
pub use mirrord_test_utils::TestProcess;
use rstest::fixture;
use tokio::{io::AsyncWriteExt, net::TcpListener, process::Command};
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan};

/// Configuration for [`Application::RustOutgoingTcp`] and [`Application::RustOutgoingUdp`].
pub const RUST_OUTGOING_PEERS: &str = "1.1.1.1:1111,2.2.2.2:2222,3.3.3.3:3333";
/// Configuration for [`Application::RustOutgoingTcp`] and [`Application::RustOutgoingUdp`].
pub const RUST_OUTGOING_LOCAL: &str = "4.4.4.4:4444";

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static MIRRORD_MACOS_ARM64_LIBRARY: OnceLock<PathBuf> = OnceLock::new();

/// Initializes tracing for the current thread, allowing us to have multiple tracing subscribers
/// writin logs to different files.
///
/// We take advantage of how Rust's thread naming scheme for tests to create the log files,
/// and if we have no thread name, then we just write the logs to `stderr`.
pub fn init_tracing() -> DefaultGuard {
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("mirrord=trace"))
        .without_time()
        .with_ansi(false)
        .with_file(true)
        .with_line_number(true)
        .with_span_events(FmtSpan::ACTIVE)
        .pretty();

    // Sets the default subscriber for the _current thread_, returning a guard that unsets
    // the default subscriber when it is dropped.
    match std::thread::current()
        .name()
        .map(|name| name.replace(':', "_"))
    {
        Some(test_name) => {
            let mut logs_file = PathBuf::from("/tmp/intproxy_logs");

            #[cfg(target_os = "macos")]
            logs_file.push("macos");
            #[cfg(not(target_os = "macos"))]
            logs_file.push("linux");

            let _ = std::fs::create_dir_all(&logs_file).ok();

            logs_file.push(&test_name);
            match File::create(&logs_file) {
                // Writes the logs to the file.
                Ok(file) => {
                    println!("Created intproxy log file: {}", logs_file.display());
                    let subscriber = subscriber.with_writer(Arc::new(file)).finish();
                    tracing::subscriber::set_default(subscriber)
                }
                // File creation failure makes the output go to `stderr`.
                Err(error) => {
                    println!(
                        "Failed to create intproxy log file at {}: {error}. Intproxy logs will be flushed to stderr",
                        logs_file.display()
                    );
                    let subscriber = subscriber.with_writer(io::stderr).finish();
                    tracing::subscriber::set_default(subscriber)
                }
            }
        }
        // No thread name makes the output go to `stderr`.
        None => {
            println!(
                "Failed to obtain current thread name, intproxy logs will be flushed to stderr"
            );
            let subscriber = subscriber.with_writer(io::stderr).finish();
            tracing::subscriber::set_default(subscriber)
        }
    }
}

/// Go versions used with test applications.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy)]
pub enum GoVersion {
    GO_1_24,
    GO_1_25,
    GO_1_26,
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

/// Various applications used by integration tests.
#[derive(Debug)]
pub enum Application {
    GoHTTP(GoVersion),
    NodeHTTP,
    PythonFastApiHTTP,
    /// Shared sockets [#864](https://github.com/metalbear-co/mirrord/issues/864).
    PythonIssue864,
    PythonFlaskHTTP,
    PythonSelfConnect,
    PythonDontLoad,
    PythonListen,
    RustFileOps,
    GoFileOps(GoVersion),
    JavaTemurinSip,
    EnvBashCat,
    NodeFileOps,
    NodeSpawn,
    NodeCopyFile,
    NodeIssue2903,
    GoDir(GoVersion),
    GoDirBypass(GoVersion),
    GoIssue834(GoVersion),
    BashShebang,
    GoRead(GoVersion),
    GoWrite(GoVersion),
    GoLSeek(GoVersion),
    GoFAccessAt(GoVersion),
    GoSelfOpen(GoVersion),
    RustOutgoingUdp,
    RustOutgoingTcp {
        non_blocking: bool,
    },
    RustIssue1123,
    RustIssue1054,
    RustIssue1458,
    RustIssue1458PortNot53,
    RustIssue1776,
    RustIssue1776PortNot53,
    RustIssue1899,
    RustIssue2001,
    RustDnsResolve,
    RustRecvFrom,
    RustListenPorts,
    Fork,
    ReadLink,
    StatfsFstatfs,
    MkdirRmdir,
    OpenFile,
    CIssue2055,
    CIssue2178,
    RustIssue2058,
    Realpath,
    NodeIssue2283,
    RustIssue2204,
    RustIssue2438,
    RustIssue3248,
    NodeIssue2807,
    RustRebind0,
    /// Go application that simply opens a file.
    GoOpen {
        /// Path to the file, accepted as `-p` param.
        path: String,
        /// Flags to use when opening the file, accepted as `-f` param.
        flags: i32,
        /// Mode to use when opening the file, accepted as `-m` param.
        mode: u32,
        version: GoVersion,
    },
    /// For running applications with the executable and arguments determined at runtime.
    DynamicApp(String, Vec<String>),
    /// Go app that only checks whether Linux pidfd syscalls are supported.
    GoIssue2988(GoVersion),
    NodeMakeConnections,
    NodeIssue3456,
    /// C++ app that dlopen c-shared go library.
    DlopenCgo,
    /// C app that calls BSD connectx(2).
    Connectx,
    /// Rust app that closes a clone socket.
    DupListen,
    /// Rust app that listens on a socket twice
    DoubleListen,
}

impl Application {
    /// Run python with shell resolving to find the actual executable.
    ///
    /// This is to help tests that run python with mirrord work locally on systems with pyenv.
    /// If we run `python3` on a system with pyenv the first executed is not python but bash. On mac
    /// that prevents the layer from loading because of SIP.
    async fn get_python3_executable() -> String {
        let mut python = Command::new("python3")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let child_stdin = python.stdin.as_mut().unwrap();
        child_stdin
            .write_all(b"import sys\nprint(sys.executable)")
            .await
            .unwrap();
        let output = python.wait_with_output().await.unwrap();
        String::from(String::from_utf8_lossy(&output.stdout).trim())
    }

    pub async fn get_executable(&self) -> String {
        match self {
            Application::PythonFlaskHTTP
            | Application::PythonSelfConnect
            | Application::PythonDontLoad
            | Application::PythonListen => Self::get_python3_executable().await,
            Application::PythonFastApiHTTP | Application::PythonIssue864 => String::from("uvicorn"),
            Application::Fork => String::from("tests/apps/fork/out.c_test_app"),
            Application::ReadLink => String::from("tests/apps/readlink/out.c_test_app"),
            Application::StatfsFstatfs => String::from("tests/apps/statfs_fstatfs/out.c_test_app"),
            Application::MkdirRmdir => String::from("tests/apps/mkdir_rmdir/out.c_test_app"),
            Application::Realpath => String::from("tests/apps/realpath/out.c_test_app"),
            Application::NodeHTTP
            | Application::NodeIssue2283
            | Application::NodeIssue2807
            | Application::NodeIssue3456 => String::from("node"),
            Application::JavaTemurinSip => format!(
                "{}/.sdkman/candidates/java/17.0.6-tem/bin/java",
                std::env::var("HOME").unwrap(),
            ),
            Application::GoHTTP(version) => format!("tests/apps/app_go/{version}.go_test_app"),
            Application::GoFileOps(version) => {
                format!("tests/apps/fileops/go/{version}.go_test_app")
            }
            Application::RustFileOps => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/fileops"
                )
            }
            Application::EnvBashCat => String::from("tests/apps/env_bash_cat.sh"),
            Application::NodeFileOps
            | Application::NodeSpawn
            | Application::NodeCopyFile
            | Application::NodeIssue2903
            | Application::NodeMakeConnections => String::from("node"),
            Application::GoDir(version) => format!("tests/apps/dir_go/{version}.go_test_app"),
            Application::GoIssue834(version) => {
                format!("tests/apps/issue834/{version}.go_test_app")
            }
            Application::GoDirBypass(version) => {
                format!("tests/apps/dir_go_bypass/{version}.go_test_app")
            }
            Application::BashShebang => String::from("tests/apps/nothing.sh"),
            Application::GoRead(version) => format!("tests/apps/read_go/{version}.go_test_app"),
            Application::GoWrite(version) => format!("tests/apps/write_go/{version}.go_test_app"),
            Application::GoLSeek(version) => format!("tests/apps/lseek_go/{version}.go_test_app"),
            Application::GoFAccessAt(version) => {
                format!("tests/apps/faccessat_go/{version}.go_test_app")
            }
            Application::GoSelfOpen(version) => {
                format!("tests/apps/self_open/{version}.go_test_app")
            }
            Application::RustIssue1123 => String::from("tests/apps/issue1123/target/issue1123"),
            Application::RustIssue1054 => String::from("tests/apps/issue1054/target/issue1054"),
            Application::RustIssue1458 => String::from("tests/apps/issue1458/target/issue1458"),
            Application::RustIssue1458PortNot53 => {
                String::from("tests/apps/issue1458portnot53/target/issue1458portnot53")
            }
            Application::RustOutgoingUdp | Application::RustOutgoingTcp { .. } => format!(
                "{}/{}",
                env!("CARGO_MANIFEST_DIR"),
                "../../target/debug/outgoing",
            ),
            Application::RustDnsResolve => format!(
                "{}/{}",
                env!("CARGO_MANIFEST_DIR"),
                "../../target/debug/dns_resolve",
            ),
            Application::RustRecvFrom => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/recv_from"
                )
            }
            Application::RustListenPorts => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/listen_ports"
                )
            }
            Application::RustIssue1776 => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/issue1776"
                )
            }
            Application::RustIssue1776PortNot53 => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/issue1776portnot53"
                )
            }
            Application::RustIssue1899 => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/issue1899"
                )
            }
            Application::RustIssue2438 => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/issue2438"
                )
            }
            Application::RustIssue2001 => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/issue2001"
                )
            }
            Application::RustIssue3248 => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/issue3248"
                )
            }
            Application::RustRebind0 => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/rebind0"
                )
            }
            Application::OpenFile => format!(
                "{}/{}",
                env!("CARGO_MANIFEST_DIR"),
                "tests/apps/open_file/out.c_test_app",
            ),
            Application::CIssue2055 => format!(
                "{}/{}",
                env!("CARGO_MANIFEST_DIR"),
                "tests/apps/gethostbyname/out.c_test_app",
            ),
            Application::CIssue2178 => format!(
                "{}/{}",
                env!("CARGO_MANIFEST_DIR"),
                "tests/apps/issue2178/out.c_test_app",
            ),
            Application::RustIssue2058 => String::from("tests/apps/issue2058/target/issue2058"),
            Application::RustIssue2204 => String::from("tests/apps/issue2204/target/issue2204"),
            Application::GoOpen { version, .. } => {
                format!("tests/apps/open_go/{version}.go_test_app")
            }
            Application::DynamicApp(exe, _) => exe.clone(),
            Application::GoIssue2988(version) => {
                format!("tests/apps/issue2988/{version}.go_test_app")
            }
            Application::DlopenCgo => String::from("tests/apps/dlopen_cgo/out.cpp_dlopen_cgo"),
            Application::Connectx => String::from("tests/apps/connectx/out.c_test_app"),
            Application::DupListen => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/dup-listen"
                )
            }
            Application::DoubleListen => {
                format!(
                    "{}/{}",
                    env!("CARGO_MANIFEST_DIR"),
                    "../../target/debug/double_listen"
                )
            }
        }
    }

    pub fn get_args(&self) -> Vec<String> {
        let mut app_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        app_path.push("tests/apps/");
        match self {
            Application::JavaTemurinSip => {
                app_path.push("java_temurin_sip/src/Main.java");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::PythonFlaskHTTP => {
                app_path.push("app_flask.py");
                println!("using flask server from {app_path:?}");
                vec![String::from("-u"), app_path.to_string_lossy().to_string()]
            }
            Application::PythonDontLoad => {
                app_path.push("dont_load.py");
                println!("using script from {app_path:?}");
                vec![String::from("-u"), app_path.to_string_lossy().to_string()]
            }
            Application::PythonListen => {
                app_path.push("app_listen.py");
                println!("using script from {app_path:?}");
                vec![String::from("-u"), app_path.to_string_lossy().to_string()]
            }
            Application::PythonFastApiHTTP => vec![
                String::from("--port=9999"),
                String::from("--host=0.0.0.0"),
                String::from("--app-dir=tests/apps/"),
                String::from("app_fastapi:app"),
            ],
            Application::PythonIssue864 => {
                vec![
                    String::from("--reload"),
                    String::from("--port=9999"),
                    String::from("--host=0.0.0.0"),
                    String::from("--app-dir=tests/apps/"),
                    String::from("shared_sockets:app"),
                ]
            }
            Application::NodeHTTP => {
                app_path.push("app_node.js");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeFileOps => {
                app_path.push("fileops.js");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeIssue3456 => {
                app_path.push("issue3456.mjs");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeSpawn => {
                app_path.push("node_spawn.mjs");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeCopyFile => {
                app_path.push("node_copyfile.mjs");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeIssue2903 => {
                app_path.push("issue2903.mjs");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeIssue2283 => {
                app_path.push("issue2883.js");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeIssue2807 => {
                app_path.push("issue2807.mjs");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::NodeMakeConnections => {
                app_path.push("make_connections.mjs");
                vec![app_path.to_string_lossy().to_string()]
            }
            Application::PythonSelfConnect => {
                app_path.push("self_connect.py");
                vec![String::from("-u"), app_path.to_string_lossy().to_string()]
            }
            Application::GoHTTP(..)
            | Application::GoDir(..)
            | Application::GoFileOps(..)
            | Application::GoIssue834(..)
            | Application::GoRead(..)
            | Application::GoWrite(..)
            | Application::GoLSeek(..)
            | Application::GoFAccessAt(..)
            | Application::Fork
            | Application::ReadLink
            | Application::StatfsFstatfs
            | Application::MkdirRmdir
            | Application::Realpath
            | Application::RustFileOps
            | Application::RustIssue1123
            | Application::RustIssue1054
            | Application::RustIssue1458
            | Application::RustIssue1458PortNot53
            | Application::RustIssue1776
            | Application::RustIssue1776PortNot53
            | Application::RustIssue1899
            | Application::RustIssue2001
            | Application::RustDnsResolve
            | Application::RustRecvFrom
            | Application::RustListenPorts
            | Application::EnvBashCat
            | Application::BashShebang
            | Application::GoSelfOpen(..)
            | Application::GoDirBypass(..)
            | Application::RustIssue2058
            | Application::OpenFile
            | Application::CIssue2055
            | Application::CIssue2178
            | Application::RustIssue2204
            | Application::RustRebind0
            | Application::RustIssue2438
            | Application::RustIssue3248
            | Application::GoIssue2988(..)
            | Application::DlopenCgo
            | Application::Connectx
            | Application::DoubleListen
            | Application::DupListen => vec![],
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
            Application::GoOpen {
                path, flags, mode, ..
            } => {
                vec![
                    "-p".to_string(),
                    path.clone(),
                    "-f".to_string(),
                    flags.to_string(),
                    "-m".to_string(),
                    mode.to_string(),
                ]
            }
            Application::DynamicApp(_, args) => args.to_owned(),
        }
    }

    pub fn get_app_port(&self) -> u16 {
        match self {
            Application::GoHTTP(..)
            | Application::GoFileOps(..)
            | Application::NodeHTTP
            | Application::RustIssue1054
            | Application::PythonFlaskHTTP
            | Application::DupListen => 80,
            // mapped from 9999 in `configs/port_mapping.json`
            Application::PythonFastApiHTTP | Application::PythonIssue864 => 1234,
            Application::RustIssue1123 => 41222,
            Application::PythonListen => 21232,
            Application::PythonDontLoad
            | Application::RustFileOps
            | Application::RustDnsResolve
            | Application::JavaTemurinSip
            | Application::EnvBashCat
            | Application::NodeFileOps
            | Application::NodeSpawn
            | Application::NodeCopyFile
            | Application::NodeIssue2903
            | Application::NodeIssue3456
            | Application::BashShebang
            | Application::Fork
            | Application::ReadLink
            | Application::StatfsFstatfs
            | Application::MkdirRmdir
            | Application::Realpath
            | Application::GoIssue834(..)
            | Application::GoRead(..)
            | Application::GoWrite(..)
            | Application::GoLSeek(..)
            | Application::GoFAccessAt(..)
            | Application::GoDirBypass(..)
            | Application::GoSelfOpen(..)
            | Application::GoDir(..)
            | Application::RustOutgoingUdp
            | Application::RustOutgoingTcp { .. }
            | Application::RustIssue1458
            | Application::RustIssue1458PortNot53
            | Application::RustIssue1776
            | Application::RustIssue1776PortNot53
            | Application::RustIssue1899
            | Application::RustIssue2001
            | Application::RustListenPorts
            | Application::RustRecvFrom
            | Application::OpenFile
            | Application::CIssue2055
            | Application::CIssue2178
            | Application::NodeIssue2283
            | Application::RustIssue2204
            | Application::RustIssue2438
            | Application::RustIssue3248
            | Application::NodeIssue2807
            | Application::RustRebind0
            | Application::GoOpen { .. }
            | Application::DynamicApp(..)
            | Application::GoIssue2988(..)
            | Application::NodeMakeConnections
            | Application::DoubleListen
            | Application::Connectx => unimplemented!("shouldn't get here"),
            Application::PythonSelfConnect => 1337,
            Application::RustIssue2058 => 1234,
            Application::DlopenCgo => 23333,
        }
    }

    /// Start the test process with the given env.
    pub async fn get_test_process(&self, env: HashMap<String, String>) -> TestProcess {
        let executable = self.get_executable().await;
        #[cfg(target_os = "macos")]
        let executable = sip_patch(&executable, SipPatchOptions::default(), None)
            .unwrap()
            .unwrap_or(executable);
        println!("Using executable: {}", &executable);
        println!("Using args: {:?}", self.get_args());
        TestProcess::start_process(executable, self.get_args(), env).await
    }

    /// Start the process of this application, with the layer lib loaded.
    /// Will start it with env from `get_env` plus whatever is passed in `extra_env_vars`.
    pub async fn start_process_with_layer(
        &self,
        dylib_path: &Path,
        extra_env_vars: Vec<(&str, &str)>,
        configuration_file: Option<&Path>,
    ) -> (TestProcess, TestIntProxy) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let env = get_env(dylib_path, address, extra_env_vars, configuration_file);
        let test_process = self.get_test_process(env).await;

        (
            test_process,
            TestIntProxy::new(listener, configuration_file).await,
        )
    }

    /// Like `start_process_with_layer`, but also verify a port subscribe.
    pub async fn start_process_with_layer_and_port(
        &self,
        dylib_path: &Path,
        extra_env_vars: Vec<(&str, &str)>,
        configuration_file: Option<&Path>,
    ) -> (TestProcess, TestIntProxy) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let env = get_env(dylib_path, address, extra_env_vars, configuration_file);
        let test_process = self.get_test_process(env).await;

        (
            test_process,
            TestIntProxy::new_with_app_port(listener, self.get_app_port(), configuration_file)
                .await,
        )
    }
}

/// Return the path to the existing layer lib, or build it first and return the path, according to
/// whether the environment variable MIRRORD_TEST_USE_EXISTING_LIB is set.
/// When testing locally the lib should be rebuilt on each run so that when developers make changes
/// they don't have to also manually build the lib before running the tests.
/// Building is slow on the CI though, so the CI can set the env var and use an artifact of an
/// earlier job on the same run (there are no code changes in between).
#[fixture]
#[once]
pub fn dylib_path() -> PathBuf {
    match std::env::var("MIRRORD_TEST_USE_EXISTING_LIB") {
        Ok(path) => {
            let dylib_path = PathBuf::from(path);
            println!("Using existing layer lib from: {dylib_path:?}");
            assert!(dylib_path.exists());
            dylib_path
        }
        Err(_) => {
            let dylib_path = test_cdylib::build_current_project();
            println!("Built library at {dylib_path:?}");
            dylib_path
        }
    }
}

/// Fixture to get configuration files directory.
#[fixture]
#[once]
pub fn config_dir() -> PathBuf {
    let mut config_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    config_path.push("tests/configs");
    config_path
}

/// Environment for the user application.
///
/// The environment includes:
/// 1. `RUST_LOG=warn,mirrord=trace`
/// 2. [`MIRRORD_LAYER_INTPROXY_ADDR`]
/// 3. Layer injection variable
/// 4. [`LayerConfig::RESOLVED_CONFIG_ENV`]
/// 6. Given `extra_vars`
///
/// `extra_vars` are also added to the [`ConfigContext`] for [`LayerConfig::resolve`].
#[allow(unused_variables)]
pub fn get_env(
    dylib_path: &Path,
    intproxy_addr: SocketAddr,
    extra_vars: Vec<(&str, &str)>,
    config_path: Option<&Path>,
) -> HashMap<String, String> {
    let extra_vars_owned = extra_vars
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<Vec<_>>();

    let mut cfg_context = ConfigContext::default()
        .override_env("MIRRORD_IMPERSONATED_TARGET", "pod/mock-target")
        .override_env("MIRRORD_REMOTE_DNS", "false")
        .override_envs(extra_vars)
        .override_env_opt(LayerConfig::FILE_PATH_ENV, config_path)
        .strict_env(true);
    let config = LayerConfig::resolve(&mut cfg_context).unwrap();

    [
        ("RUST_LOG".to_string(), "warn,mirrord=debug".to_string()),
        (
            MIRRORD_LAYER_INTPROXY_ADDR.to_string(),
            intproxy_addr.to_string(),
        ),
        (
            LayerConfig::RESOLVED_CONFIG_ENV.to_string(),
            config.encode().unwrap(),
        ),
        #[cfg(target_os = "macos")]
        (
            "DYLD_INSERT_LIBRARIES".to_string(),
            dylib_path.to_str().unwrap().to_string(),
        ),
        #[cfg(target_os = "linux")]
        (
            "LD_PRELOAD".to_string(),
            dylib_path.to_str().unwrap().to_string(),
        ),
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        (
            // The universal layer library loads arm64 library from the path specified in this
            // environment variable when the host runs arm64e.
            // See `mirorrd/layer/shim.c` for more information.
            "MIRRORD_MACOS_ARM64_LIBRARY".to_string(),
            arm64_dylib_path().to_str().unwrap().to_string(),
        ),
    ]
    .into_iter()
    .chain(extra_vars_owned)
    .collect()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn arm64_dylib_path() -> &'static Path {
    MIRRORD_MACOS_ARM64_LIBRARY
        .get_or_init(|| {
            if let Ok(path) = std::env::var("MIRRORD_MACOS_ARM64_LIBRARY") {
                let dylib_path = PathBuf::from(path);
                println!("Using existing macOS arm64 layer lib from: {dylib_path:?}");
                assert!(dylib_path.exists());
                return dylib_path;
            }

            if let Ok(path) = std::env::var("MIRRORD_TEST_USE_EXISTING_LIB") {
                let derived_path = path.replace("universal-apple-darwin", "aarch64-apple-darwin");
                let dylib_path = PathBuf::from(&derived_path);
                if dylib_path.exists() {
                    return dylib_path;
                } else {
                    println!("Derived arm64 layer lib path does not exist: {derived_path}");
                }
            }

            let dylib_path = test_cdylib::build_current_project();
            println!("Built macOS arm64 layer lib at {dylib_path:?}");
            dylib_path
        })
        .as_path()
}
