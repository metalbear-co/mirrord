use std::{
    collections::HashMap,
    fmt::{self, Debug},
    fs::File,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use mirrord_config::MIRRORD_TEST_INTPROXY_ADDR;
pub use mirrord_layer_tests::intproxy::TestIntProxy;
pub use mirrord_test_utils::{TestProcess, node, python, run_command::run_exec};
use rstest::fixture;
use tokio::net::TcpListener;
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan};
/// Returns the absolute path of the file at `path` in this package, like `configs/fs.json`,
/// from the test's runfiles.
///
/// Most apps in [`Application::get_executable`] run from a relative path instead, which starts
/// from the working directory: nextest runs the tests in this package's directory in the runfiles.
pub fn package_file(path: &str) -> PathBuf {
    mirrord_runfiles::runfile(&format!("layer-tests/{path}"))
}

/// Returns the absolute path of the script at `path` in `apps`, like `app-flask/main.py`, from
/// the test's runfiles.
pub fn app_script(path: &str) -> String {
    package_file(&format!("apps/{path}"))
        .to_string_lossy()
        .into_owned()
}

/// Configuration for [`Application::RustOutgoingTcp`] and [`Application::RustOutgoingUdp`].
pub const RUST_OUTGOING_PEERS: &str = "1.1.1.1:1111,2.2.2.2:2222,3.3.3.3:3333";
/// Configuration for [`Application::RustOutgoingTcp`] and [`Application::RustOutgoingUdp`].
pub const RUST_OUTGOING_LOCAL: &str = "4.4.4.4:4444";
pub const COR_1401_SEQPACKET_SOCKET: &str = "/tmp/ochorowicz.sock";

/// Initializes tracing for the current thread, allowing us to have multiple tracing subscribers
/// writing logs to different files.
///
/// We take advantage of how Rust's thread naming scheme for tests to create the log files,
/// and if we have no thread name, then we just write the logs to `stderr`.
pub fn init_tracing() -> DefaultGuard {
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new("mirrord=info"))
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
            // Bazel deletes a test's `/tmp`, but keeps `TEST_UNDECLARED_OUTPUTS_DIR`, in the
            // test's `test.outputs` in `bazel-testlogs`.
            let mut logs_file = std::env::var_os("TEST_UNDECLARED_OUTPUTS_DIR")
                .map(|outputs| PathBuf::from(outputs).join("intproxy_logs"))
                .unwrap_or_else(|| PathBuf::from("/tmp/intproxy_logs"));

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
    GO_1_25,
    GO_1_26,
    GO_1_27,
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
    PythonSocketPair,
    PythonCor1401Seqpacket,
    PythonDontLoad,
    PythonListen,
    RustFileOps,
    GoFileOps(GoVersion),
    EnvBashCat,
    NodeFileOps,
    NodeSpawn,
    NodeCopyFile,
    NodeStat,
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
    Cor1734FileMode,
    SendfileEagain,
    SendfilePartial,
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
    /// Go app that maps an anonymous page with a raw `mmap` syscall and writes to it.
    GoIssue5092(GoVersion),
    NodeMakeConnections,
    NodeIssue3456,
    /// Node app started via `sh` with stdin closed before the `exec`, reproducing how Next.js
    /// with Turbopack spawns its workers.
    /// See [#4622](https://github.com/metalbear-co/mirrord/issues/4622).
    NodeIssue4622,
    /// C++ app that dlopen c-shared go library.
    DlopenCgo,
    /// C app that calls BSD connectx(2).
    Connectx,
    /// C app that resolves names through macOS DNS-SD, like Bun does.
    DnsSd,
    /// Rust app that closes a clone socket.
    DupListen,
    /// Rust app that spawns children which must inherit socket metadata.
    PosixSpawnSharedSockets,
    /// Rust app that listens on a socket twice
    DoubleListen,
    /// C app that calls `connect(2)` on a unix socket with a too-large `addrlen`
    /// so that `sun_path` carries trailing null bytes.
    UnixConnectAddrlen,
}

impl Application {
    pub fn get_executable(&self) -> String {
        match self {
            Application::PythonFlaskHTTP
            | Application::PythonSelfConnect
            | Application::PythonSocketPair
            | Application::PythonCor1401Seqpacket
            | Application::PythonDontLoad
            | Application::PythonListen
            | Application::PythonFastApiHTTP
            | Application::PythonIssue864 => python(),
            Application::Fork => String::from("apps/fork/fork"),
            Application::ReadLink => String::from("apps/readlink/readlink"),
            Application::StatfsFstatfs => String::from("apps/statfs-fstatfs/statfs-fstatfs"),
            Application::MkdirRmdir => String::from("apps/mkdir-rmdir/mkdir-rmdir"),
            Application::Cor1734FileMode => {
                String::from("apps/cor-1734-file-mode/cor-1734-file-mode.out")
            }
            Application::SendfileEagain => String::from("apps/sendfile-eagain/sendfile-eagain"),
            Application::SendfilePartial => String::from("apps/sendfile-partial/sendfile-partial"),
            Application::Realpath => String::from("apps/realpath/realpath"),
            Application::NodeHTTP
            | Application::NodeIssue2283
            | Application::NodeIssue2807
            | Application::NodeIssue3456 => node(),
            Application::GoHTTP(version) => format!("apps/app-go/app-go.{version}"),
            Application::GoFileOps(version) => {
                format!("apps/fileops/go/fileops.{version}")
            }
            Application::RustFileOps => String::from("apps/fileops/rust/fileops"),
            Application::EnvBashCat => String::from("apps/env-bash-cat/main.sh"),
            Application::NodeFileOps
            | Application::NodeSpawn
            | Application::NodeCopyFile
            | Application::NodeStat
            | Application::NodeIssue2903
            | Application::NodeMakeConnections => node(),
            Application::GoDir(version) => format!("apps/dir-go/dir-go.{version}"),
            Application::GoIssue834(version) => {
                format!("apps/issue834/issue834.{version}")
            }
            Application::GoDirBypass(version) => {
                format!("apps/dir-go-bypass/dir-go-bypass.{version}")
            }
            Application::BashShebang => String::from("apps/nothing/main.sh"),
            Application::GoRead(version) => format!("apps/read-go/read-go.{version}"),
            Application::GoWrite(version) => format!("apps/write-go/write-go.{version}"),
            Application::GoLSeek(version) => format!("apps/lseek-go/lseek-go.{version}"),
            Application::GoFAccessAt(version) => {
                format!("apps/faccessat-go/faccessat-go.{version}")
            }
            Application::GoSelfOpen(version) => {
                format!("apps/self-open/self-open.{version}")
            }
            Application::RustIssue1123 => String::from("apps/issue1123/issue1123"),
            Application::RustIssue1054 => String::from("apps/issue1054/issue1054"),
            Application::RustIssue1458 => String::from("apps/issue1458/issue1458"),
            Application::RustIssue1458PortNot53 => {
                String::from("apps/issue1458portnot53/issue1458portnot53")
            }
            Application::RustOutgoingUdp | Application::RustOutgoingTcp { .. } => {
                String::from("apps/outgoing/outgoing")
            }
            Application::RustDnsResolve => String::from("apps/dns-resolve/dns-resolve"),
            Application::RustRecvFrom => String::from("apps/recv-from/recv-from"),
            Application::RustListenPorts => String::from("apps/listen-ports/listen-ports"),
            Application::RustIssue1776 => String::from("apps/issue1776/issue1776"),
            Application::RustIssue1776PortNot53 => {
                String::from("apps/issue1776portnot53/issue1776portnot53")
            }
            Application::RustIssue1899 => String::from("apps/issue1899/issue1899"),
            Application::RustIssue2438 => String::from("apps/issue2438/issue2438"),
            Application::RustIssue2001 => String::from("apps/issue2001/issue2001"),
            Application::RustIssue3248 => String::from("apps/issue3248/issue3248"),
            Application::RustRebind0 => String::from("apps/rebind0/rebind0"),
            Application::OpenFile => package_file("apps/open-file/open-file")
                .to_string_lossy()
                .into_owned(),
            Application::CIssue2055 => package_file("apps/gethostbyname/gethostbyname")
                .to_string_lossy()
                .into_owned(),
            Application::CIssue2178 => package_file("apps/issue2178/issue2178")
                .to_string_lossy()
                .into_owned(),
            Application::RustIssue2058 => String::from("apps/issue2058/issue2058"),
            Application::RustIssue2204 => String::from("apps/issue2204/issue2204"),
            Application::GoOpen { version, .. } => {
                format!("apps/open-go/open-go.{version}")
            }
            Application::DynamicApp(exe, _) => exe.clone(),
            Application::NodeIssue4622 => String::from("sh"),
            Application::GoIssue2988(version) => {
                format!("apps/issue2988/issue2988.{version}")
            }
            Application::GoIssue5092(version) => {
                format!("apps/issue5092/issue5092.{version}")
            }
            Application::DlopenCgo => String::from("apps/dlopen-cgo/dlopen-cgo"),
            Application::Connectx => String::from("apps/connectx/connectx"),
            Application::DnsSd => String::from("apps/dns-sd/dns-sd"),
            Application::DupListen => String::from("apps/dup-listen/dup-listen"),
            Application::PosixSpawnSharedSockets => {
                String::from("apps/posix-spawn-shared-sockets/posix-spawn-shared-sockets")
            }
            Application::DoubleListen => String::from("apps/double-listen/double-listen"),
            Application::UnixConnectAddrlen => {
                package_file("apps/unix-connect-addrlen/unix-connect-addrlen")
                    .to_string_lossy()
                    .into_owned()
            }
        }
    }

    pub fn get_args(&self) -> Vec<String> {
        match self {
            Application::PythonFlaskHTTP => {
                vec![String::from("-u"), app_script("app-flask/main.py")]
            }
            Application::PythonDontLoad => {
                vec![String::from("-u"), app_script("dont_load/main.py")]
            }
            Application::PythonListen => vec![String::from("-u"), app_script("app-listen/main.py")],
            // Bazel's Python environment has no `uvicorn` program, so uvicorn runs as a module.
            Application::PythonFastApiHTTP => vec![
                String::from("-m"),
                String::from("uvicorn"),
                String::from("--port=9999"),
                String::from("--host=0.0.0.0"),
                String::from("--app-dir=apps/app-fastapi"),
                String::from("main:app"),
            ],
            Application::PythonIssue864 => vec![
                "-m".to_owned(),
                "uvicorn".to_owned(),
                "--reload".to_owned(),
                "--port=18064".to_owned(),
                "--host=0.0.0.0".to_owned(),
                "--app-dir=apps/shared-sockets".to_owned(),
                "main:app".to_owned(),
            ],
            // Node resolves a script's imports, like `express`, from the script's real path, which
            // is outside the runfiles. With the main script's symlink kept, Node resolves them from
            // the runfiles, where the app's `js_binary` puts the packages that it imports. Only the
            // main script keeps its symlink, because the packages' own imports must still resolve
            // from their real paths, in the pnpm store that rules_js lays out.
            Application::NodeHTTP => vec![
                "--preserve-symlinks-main".to_owned(),
                app_script("app-node/main.js"),
            ],
            Application::NodeFileOps => vec![app_script("fileops/node/main.js")],
            Application::NodeIssue3456 => vec![app_script("issue3456/main.mjs")],
            Application::NodeSpawn => vec![app_script("node-spawn/main.mjs")],
            Application::NodeCopyFile => vec![app_script("node-copyfile/main.mjs")],
            Application::NodeStat => vec![app_script("node-stat/main.mjs")],
            Application::NodeIssue2903 => vec![app_script("issue2903/main.mjs")],
            Application::NodeIssue2283 => vec![app_script("issue2283/main.js")],
            // Imports `portfinder`, as `NodeHTTP` imports `express`.
            Application::NodeIssue2807 => vec![
                "--preserve-symlinks-main".to_owned(),
                app_script("issue2807/main.mjs"),
            ],
            Application::NodeMakeConnections => vec![app_script("make-connections/main.mjs")],
            Application::PythonSelfConnect => {
                vec![String::from("-u"), app_script("self-connect/main.py")]
            }
            Application::PythonSocketPair => {
                vec![String::from("-u"), app_script("socketpair/main.py")]
            }
            Application::PythonCor1401Seqpacket => {
                vec![
                    String::from("-u"),
                    app_script("cor-1401-seqpacket/main.py"),
                    COR_1401_SEQPACKET_SOCKET.to_owned(),
                ]
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
            | Application::Cor1734FileMode
            | Application::SendfileEagain
            | Application::SendfilePartial
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
            | Application::GoIssue5092(..)
            | Application::DlopenCgo
            | Application::Connectx
            | Application::DnsSd
            | Application::DoubleListen
            | Application::DupListen
            | Application::UnixConnectAddrlen => vec![],
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
                    "-p".to_owned(),
                    path.clone(),
                    "-f".to_owned(),
                    flags.to_string(),
                    "-m".to_owned(),
                    mode.to_string(),
                ]
            }
            Application::DynamicApp(_, args) => args.to_owned(),
            Application::NodeIssue4622 => vec![
                "-c".to_owned(),
                format!(
                    "exec 0<&- ; exec {} {}",
                    node(),
                    app_script("issue4622/main.js")
                ),
            ],
            Application::PosixSpawnSharedSockets => vec![],
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
            // Mapped from 9999 in `configs/port_mapping.json`.
            Application::PythonFastApiHTTP => 1234,
            // A separate local port prevents reload startup from racing other test listeners.
            // Mapped from 18064 in `configs/port_mapping_shared_sockets.toml`.
            Application::PythonIssue864 => 1234,
            Application::RustIssue1123 => 41222,
            Application::PythonListen => 21232,
            Application::PythonDontLoad
            | Application::RustFileOps
            | Application::RustDnsResolve
            | Application::EnvBashCat
            | Application::NodeFileOps
            | Application::NodeSpawn
            | Application::NodeCopyFile
            | Application::NodeStat
            | Application::NodeIssue2903
            | Application::NodeIssue3456
            | Application::BashShebang
            | Application::Fork
            | Application::ReadLink
            | Application::StatfsFstatfs
            | Application::MkdirRmdir
            | Application::Cor1734FileMode
            | Application::SendfileEagain
            | Application::SendfilePartial
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
            | Application::NodeIssue4622
            | Application::GoIssue2988(..)
            | Application::GoIssue5092(..)
            | Application::NodeMakeConnections
            | Application::DoubleListen
            | Application::PosixSpawnSharedSockets
            | Application::PythonSocketPair
            | Application::PythonCor1401Seqpacket
            | Application::UnixConnectAddrlen
            | Application::Connectx
            | Application::DnsSd => unimplemented!("shouldn't get here"),
            Application::PythonSelfConnect => 1337,
            Application::RustIssue2058 => 1234,
            Application::DlopenCgo => 23333,
        }
    }

    pub async fn get_test_process(
        &self,
        env: HashMap<String, String>,
        config_path: Option<&Path>,
    ) -> TestProcess {
        let executable = self.get_executable();
        println!("Using executable: {executable}");
        println!("Using args: {:?}", self.get_args());

        let cli_args_owned: Option<Vec<String>> = config_path.map(|path| {
            vec![
                "--config-file".to_owned(),
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
        run_exec(cmdline, None, None, cli_args, Some(env_refs), None).await
    }

    pub async fn start_process(
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

    /// Like `start_process`, but also verify a port subscribe.
    pub async fn start_process_with_port(
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
    package_file("configs")
}

/// Environment for the user application.
///
/// The environment includes:
/// 1. `MIRRORD_IMPERSONATED_TARGET=pod/mock-target` (to run exec NOT in `targetless` mode)
/// 2. `MIRRORD_REMOTE_DNS=false` (no remote environment to do remote_dns to -.-)
/// 3. `MIRRORD_TEST_INTPROXY_ADDR` (for `mirrord exec` test harness)
/// 4. `MIRRORD_FILE_MODE=local` on Windows
/// 5. Given `extra_vars` (can override any of the above)
/// 6. Strict CLI environment via `MIRRORD_CLI_STRICT_ENV=true` and an allowlist that contains all
///    keys from the defaults and `extra_vars`
pub fn get_env(
    intproxy_addr: SocketAddr,
    extra_vars: Vec<(&str, &str)>,
) -> HashMap<String, String> {
    let extra_vars_owned = extra_vars
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<Vec<_>>();
    let mut default_env = vec![
        (
            "MIRRORD_IMPERSONATED_TARGET".to_owned(),
            "pod/mock-target".to_owned(),
        ),
        ("MIRRORD_REMOTE_DNS".to_owned(), "false".to_owned()),
        (
            MIRRORD_TEST_INTPROXY_ADDR.to_owned(),
            intproxy_addr.to_string(),
        ),
    ];
    if cfg!(windows) {
        // on windows default to local file_mode to prevent accidental TestIntproxy failure due to
        // remote-first read approach, implicitly overriden through `extra_vars` for tests that
        // require it.
        default_env.push(("MIRRORD_FILE_MODE".to_owned(), "local".to_owned()));
    }

    let mut exec_env_map: HashMap<String, String> =
        default_env.into_iter().chain(extra_vars_owned).collect();
    // Make CLI ConfigContext strict
    // except for the above default env vars + extra env vars provided
    exec_env_map.extend([
        ("MIRRORD_CLI_STRICT_ENV".to_owned(), "true".to_owned()),
        (
            "MIRRORD_CLI_STRICT_ENV_ALLOWLIST".to_owned(),
            exec_env_map.keys().cloned().collect::<Vec<_>>().join(","),
        ),
    ]);
    // Don't add additional env vars past this MIRRORD_CLI_STRICT_ENV line

    exec_env_map
}
