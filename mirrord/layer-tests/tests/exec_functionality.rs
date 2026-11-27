#[cfg(unix)]
use std::{fs::OpenOptions, io::Write, net::SocketAddr, ops::Not, path::PathBuf, time::Duration};

use rstest::rstest;
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::{io::copy_bidirectional, net::TcpStream};

mod common;
pub use common::*;

/// Runs `python3 tests/apps/<script>` under `mirrord exec` in `simple` progress mode, with the
/// layer connecting to `intproxy`.
#[cfg(unix)]
async fn exec_python(
    intproxy: SocketAddr,
    script: &str,
    extra_env: Vec<(&str, &str)>,
) -> TestProcess {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/apps")
        .join(script);
    let env = get_env(
        intproxy,
        [
            ("MIRRORD_FILE_MODE", "local"),
            ("MIRRORD_PROGRESS_MODE", "simple"),
        ]
        .into_iter()
        .chain(extra_env)
        .collect(),
    );
    let env = env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let cmdline = vec![
        Application::get_python3_executable().await,
        script.to_str().unwrap().to_owned(),
    ];

    run_exec(cmdline, None, None, None, Some(env), None).await
}

/// Ensures that `mirrord exec` extracts the layer when `MIRRORD_LAYER_FILE` is not set.
#[rstest]
#[tokio::test]
async fn exec_extracts_layer_without_env() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let env = get_env(address, vec![("MIRRORD_FILE_MODE", "local")]);

    let env_pairs: Vec<(String, String)> = env.into_iter().collect();
    let env_refs: Vec<(&str, &str)> = env_pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let executable = Application::get_python3_executable().await;
    let cmdline = vec![executable, "-c".to_owned(), "print('hi')".to_owned()];

    let mut process = run_exec(
        cmdline,
        None,
        None,
        None,
        Some(env_refs),
        Some(&["MIRRORD_LAYER_FILE"]),
    )
    .await;

    let mut intproxy = TestIntProxy::new(listener, None).await;

    process.wait_assert_success().await;
    process.assert_stdout_contains("hi").await;
    process
        .assert_stderr_contains("MIRRORD_LAYER_FILE not set, extracting library from binary")
        .await;
    if let Some(message) = intproxy.try_recv().await {
        panic!("unexpected message from layer: {message:?}");
    }
}

/// `Ready!` means the layer in the program has started, so `mirrord exec` prints it only once the
/// layer's session with the internal proxy is through.
#[cfg(unix)]
#[tokio::test]
async fn exec_prints_ready_only_once_the_layer_is_connected() {
    // The layer connects to `gate`, which holds its session back until the test forwards it.
    let gate = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let intproxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let intproxy_address = intproxy_listener.local_addr().unwrap();
    let mut process = exec_python(gate.local_addr().unwrap(), "up_greeter.py", vec![]).await;

    let (layer_stream, _) = tokio::time::timeout(Duration::from_secs(30), gate.accept())
        .await
        .expect("the layer never connected")
        .unwrap();
    assert!(
        process.get_stdout().await.contains("Ready!").not(),
        "Ready! came before the layer's session was accepted"
    );

    let _intproxy = TestIntProxy::new(intproxy_listener, None).await;
    // Every layer session goes through, not only the first: on macOS, a framework Python's
    // `python3` is a launcher that `execve`s the real interpreter, whose layer connects again.
    tokio::spawn(async move {
        let mut layer_stream = layer_stream;
        loop {
            let mut intproxy_stream = TcpStream::connect(intproxy_address).await.unwrap();
            tokio::spawn(async move {
                let _ = copy_bidirectional(&mut layer_stream, &mut intproxy_stream).await;
            });
            match gate.accept().await {
                Ok((next, _)) => layer_stream = next,
                Err(_) => break,
            }
        }
    });

    process.wait_assert_success().await;
    let stdout = process.get_stdout().await;
    let ready = stdout.find("Ready!").expect("mirrord exec printed Ready!");
    let greeting = stdout
        .find("hello from mirrord up")
        .expect("the program ran");
    assert!(
        ready < greeting,
        "Ready! comes before the program's own output:\n{stdout}"
    );
}

/// What `tests/apps/exec_debug_hold.py` prints once it runs.
#[cfg(unix)]
const HELD_PROGRAM_RAN: &str = "the held program runs";

/// What the child `tests/apps/exec_debug_hold.py` starts prints once it runs.
#[cfg(unix)]
const CHILD_RAN: &str = "its child runs";

/// The diagnostic port `mirrord up` would hand the held program's `mirrord exec`.
#[cfg(unix)]
const DOTNET_PORT: &str = "/tmp/mirrord-debug-0/dotnet-1-layertest.sock";

/// Starts `tests/apps/exec_debug_hold.py` under `mirrord exec`, held for a debugger the way
/// `mirrord up --debug` asks for it, and waits for the hold's announcement.
///
/// Returns the process, its intproxy, and the release path the announcement names.
#[cfg(unix)]
#[allow(clippy::indexing_slicing)]
async fn start_held_program() -> (TestProcess, TestIntProxy, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let process = exec_python(
        listener.local_addr().unwrap(),
        "exec_debug_hold.py",
        vec![
            ("MIRRORD_DEBUG_HOLD", "1"),
            ("MIRRORD_DEBUG_DOTNET_PORT", DOTNET_PORT),
        ],
    )
    .await;
    let intproxy = TestIntProxy::new(listener, None).await;

    let release = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            // Only a whole line: the path may still be arriving.
            let release = held_announcement(&process.get_stdout().await)
                .and_then(|hold| hold["release"].as_str().map(str::to_owned));
            match release {
                Some(release) => break release,
                None => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
    .await
    .expect("the held program never announced its hold");

    (process, intproxy, release)
}

/// The JSON of the first whole `debug-hold {json}` line in `stdout`, as an IDE reads it.
#[cfg(unix)]
fn held_announcement(stdout: &str) -> Option<serde_json::Value> {
    stdout
        .split_inclusive('\n')
        .filter_map(|line| line.strip_suffix('\n')?.strip_prefix("debug-hold "))
        .find_map(|json| serde_json::from_str(json).ok())
}

/// What an IDE does once its debugger is attached: write one byte to the announced release.
#[cfg(unix)]
fn release(path: &str) {
    OpenOptions::new()
        .write(true)
        .open(path)
        .expect("the held program keeps its release open for reading")
        .write_all(b"x")
        .expect("the release accepts a byte");
}

/// A program held for a debugger announces its release after `Ready!`, under its own pid, and
/// runs only once a byte is written to the release.
#[cfg(unix)]
#[tokio::test]
#[allow(clippy::indexing_slicing)]
async fn exec_holds_the_program_until_its_release_gets_a_byte() {
    let (mut process, _intproxy, release_path) = start_held_program().await;

    let stdout = process.get_stdout().await;
    let ready = stdout.find("Ready!").expect("the layer printed Ready!");
    let announcement = stdout.find("debug-hold ").unwrap();
    assert!(
        ready < announcement,
        "Ready! comes before the hold is announced:\n{stdout}"
    );
    // `execve` keeps `mirrord exec`'s pid, so the program holds under it.
    let pid = process.child.id().unwrap();
    let hold = held_announcement(&stdout).expect("announced");
    assert_eq!(
        hold["pid"], pid,
        "the program is held under its own pid, {pid}:\n{stdout}"
    );
    assert_eq!(hold["trigger"], "process", "{stdout}");

    let ran_while_held = tokio::time::timeout(Duration::from_secs(1), async {
        while process.get_stdout().await.contains(HELD_PROGRAM_RAN).not() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        ran_while_held.is_err(),
        "the program ran before the IDE released it"
    );

    release(&release_path);

    process.wait_assert_success().await;
    process.assert_stdout_contains(HELD_PROGRAM_RAN).await;
}

/// Only the program is held: a child it starts runs at once, even one handed the program's hold
/// binding, and prints no `Ready!` of its own, even one handed the program's `Ready!` binding.
///
/// The child still inherits the diagnostic port through the `execve` hook, so a .NET program a
/// held program starts pauses at its runtime's startup. So does a child whose environment sets
/// ports of its own, as a launch profile does: the port joins them, last.
#[cfg(unix)]
#[tokio::test]
async fn a_child_of_a_held_program_is_never_held() {
    let (mut process, _intproxy, release_path) = start_held_program().await;

    release(&release_path);

    // A held child would wait for a release that never comes.
    tokio::time::timeout(Duration::from_secs(60), process.wait_assert_success())
        .await
        .expect("the program's child was held too");
    let stdout = process.get_stdout().await;
    assert!(stdout.contains(CHILD_RAN), "the child ran:\n{stdout}");
    assert!(
        stdout.contains(&format!(
            "its child's DOTNET_DiagnosticPorts: {DOTNET_PORT},connect,suspend\n"
        )),
        "the child inherits the port exec merged in:\n{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "its child's DOTNET_DiagnosticPorts: theirs,nosuspend;{DOTNET_PORT},connect,suspend\n"
        )),
        "a child with ports of its own gets the port after them:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("debug-hold ").count(),
        1,
        "only the program announces a hold:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("Ready!").count(),
        1,
        "only the program prints Ready!:\n{stdout}"
    );
}

/// With `feature.env.load_from_process`, the layer fetches the pod's environment itself, and
/// drops the pod's .NET diagnostic ports as `exec`'s own fetch does: they name sockets inside the
/// pod, and one with `suspend` keeps a local runtime paused for good. The user's own values stay,
/// whether set locally, in the env file or in `env.override`, and the rest of the pod's
/// environment arrives.
#[cfg(unix)]
#[tokio::test]
async fn the_layers_own_env_fetch_drops_only_the_pods_diagnostic_ports() {
    use mirrord_protocol::{ClientMessage, DaemonMessage, RemoteEnvVars};

    let dir = tempfile::tempdir().unwrap();
    let env_file = dir.path().join("app.env");
    std::fs::write(&env_file, "COMPlus_DiagnosticPorts=from-env-file\n").unwrap();
    let config = dir.path().join("mirrord.json");
    std::fs::write(
        &config,
        serde_json::json!({
            "feature": {
                "env": {
                    "load_from_process": true,
                    "env_file": env_file,
                    "override": { "DOTNET_DefaultDiagnosticPortSuspend": "0" },
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    let pod = [
        ("DOTNET_DiagnosticPorts", "/pod/monitor.sock,suspend"),
        ("COMPlus_DiagnosticPorts", "/pod/monitor.sock,suspend"),
        ("DOTNET_DefaultDiagnosticPortSuspend", "1"),
        ("COMPlus_DefaultDiagnosticPortSuspend", "1"),
        // The drop matches names without case, as the CLI's does.
        ("dotnet_diagnosticports", "/pod/other.sock"),
        ("DOTNET_gcServer", "1"),
    ];
    // What the program reads of each, `None` for unset.
    let expected = [
        ("DOTNET_DiagnosticPorts", "mine,nosuspend"),
        ("COMPlus_DiagnosticPorts", "from-env-file"),
        ("DOTNET_DefaultDiagnosticPortSuspend", "0"),
        ("COMPlus_DefaultDiagnosticPortSuspend", "None"),
        ("dotnet_diagnosticports", "None"),
        ("DOTNET_gcServer", "1"),
    ];

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let env = get_env(
        listener.local_addr().unwrap(),
        vec![
            ("MIRRORD_FILE_MODE", "local"),
            ("DOTNET_DiagnosticPorts", "mine,nosuspend"),
        ],
    );
    let env = env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let names = expected.map(|(name, _)| name);
    let cmdline = vec![
        Application::get_python3_executable().await,
        "-c".to_owned(),
        format!(
            "import os\nfor name in {names:?}:\n    print(f'{{name}}={{os.environ.get(name)}}')"
        ),
    ];
    let mut process = run_exec(
        cmdline,
        None,
        None,
        Some(vec!["--config-file", config.to_str().unwrap()]),
        Some(env),
        None,
    )
    .await;
    let mut intproxy = TestIntProxy::new(listener, None).await;

    match intproxy.recv().await {
        ClientMessage::GetEnvVarsRequest(..) => {}
        other => panic!("the layer fetches the pod's environment first, not {other:?}"),
    }
    intproxy
        .send(DaemonMessage::GetEnvVarsResponse(Ok(RemoteEnvVars(
            pod.iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        ))))
        .await;

    process.wait_assert_success().await;
    let stdout = process.get_stdout().await;
    for (name, value) in expected {
        assert!(
            stdout.contains(&format!("{name}={value}\n")),
            "the program reads {name}={value}:\n{stdout}"
        );
    }
}
