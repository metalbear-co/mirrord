#![cfg(target_os = "linux")]

mod common;

use std::{io::Write, net::TcpListener, ops::Not, os::fd::AsRawFd, process::Stdio, time::Duration};

pub use common::*;
use mirrord_config::{
    LayerConfig, LayerFileConfig,
    config::{ConfigContext, MirrordConfig},
};
use mirrord_protocol::{
    ClientMessage, DaemonMessage,
    local_share::{LocalShareClientMessage, LocalShareEvent},
    tcp::{DaemonTcp, LayerTcpSteal, StealType},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpListener as AsyncTcpListener,
    time::timeout,
};

// Failure snapshots must not drain a pipe while testing a share failure under backpressure.
async fn queued_output(pipe: &mut (impl AsyncRead + AsRawFd + Unpin)) -> String {
    let mut queued: libc::c_int = 0;
    // SAFETY: FIONREAD writes one c_int for the borrowed, live descriptor.
    let result = unsafe { libc::ioctl(pipe.as_raw_fd(), libc::FIONREAD, &mut queued) };
    if result != 0 {
        return format!("FIONREAD failed: {}", std::io::Error::last_os_error());
    }
    let mut bytes = vec![0; queued.clamp(0, 8192) as usize];
    let read = timeout(Duration::from_secs(1), pipe.read_exact(&mut bytes)).await;
    format!(
        "queued={queued}, read={read:?}, output={}",
        String::from_utf8_lossy(&bytes)
    )
}

/// The application prints that the link waits for it to listen, then the link once, in the progress
/// mode `mirrord exec` runs in, and then the failure that ends the session, while it is idle in
/// accept.
///
/// `mirrord exec` starts the application with progress output off, so the layer prints in the mode
/// that `exec` itself ran in: `json` lines on stdout, where `Ready!` goes too, and simple text on
/// stderr.
#[rstest::rstest]
#[case("simple", false)]
#[case("json", false)]
#[case("simple", true)]
#[tokio::test]
async fn local_share_prints_ready_and_failure(#[case] mode: &str, #[case] fork: bool) {
    let scenario = tempfile::TempDir::new().unwrap();
    let tmpdir = scenario.path().to_str().unwrap();
    let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
    let local_port = reserved.local_addr().unwrap().port();
    drop(reserved);
    let mut config = tempfile::NamedTempFile::with_suffix(".toml").unwrap();
    write!(
        config,
        r#"share = true
key = "local-share-integration"
[feature]
env = false
fs = "local"
[feature.network]
outgoing = false
dns = false
[feature.network.incoming]
mode = "steal"
ports = [9000, 8080]
port_mapping = [[{local_port}, 9000]]
"#
    )
    .unwrap();
    let fork_literal = if fork { "True" } else { "False" };
    let app = Application::DynamicApp(
        "python3".to_owned(),
        vec![
            "-c".to_owned(),
            format!(
                "import os,socket,sys
s=socket.socket()
s.bind(('127.0.0.1',{local_port}))
s.listen()
print('APP_'+'LISTENING',flush=True)
if {fork_literal}:
 sys.stdin.readline()
 child=os.fork()
 if child:
  sys.stdin.readline()
  s.close()
  print('PARENT_'+'CLOSED',flush=True)
  os.waitpid(child,0)
 else:
  print('CHILD_'+'REGISTERED',flush=True)
  s.accept()
else:
 s.accept()"
            ),
        ],
    );
    let json = mode == "json";
    let waiting =
        format!("Waiting for your application to listen on port {local_port} to share it.");
    let (mut process, mut proxy) = app
        .start_process(
            vec![("MIRRORD_PROGRESS_MODE", mode), ("TMPDIR", tmpdir)],
            Some(config.path()),
        )
        .await;
    let start = timeout(Duration::from_secs(15), proxy.try_recv()).await;
    assert!(
        matches!(
            start,
            Ok(Some(ClientMessage::LocalShare(
                LocalShareClientMessage::Start
            )))
        ),
        "prints mode={mode} fork={fork} stage=Start: actual={start:?}, stdout={:.8192}, stderr={:.8192}",
        process.get_stdout().await,
        process.get_stderr().await
    );
    proxy
        .send(DaemonMessage::LocalShare(LocalShareEvent::Accepted {
            entry_port: 9000,
        }))
        .await;
    let subscription = timeout(Duration::from_secs(5), proxy.try_recv()).await;
    assert!(
        matches!(
            subscription,
            Ok(Some(ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(
                StealType::FilteredHttpEx(9000, _)
            ))))
        ),
        "prints mode={mode} fork={fork} stage=subscription: actual={subscription:?}, stdout={:.8192}, stderr={:.8192}",
        process.get_stdout().await,
        process.get_stderr().await
    );
    // The application listens on the mapped local port, so that is the port it is told about.
    if json {
        process
            .wait_for_line_stdout(Duration::from_secs(5), &waiting)
            .await;
    } else {
        process
            .wait_for_line(Duration::from_secs(5), &waiting)
            .await;
    }
    for output in [process.get_stdout().await, process.get_stderr().await] {
        assert!(output.contains("https://test.example").not(), "{output}");
    }
    proxy
        .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(
            9000,
        ))))
        .await;
    process
        .wait_for_line_stdout(Duration::from_secs(5), "APP_LISTENING")
        .await;
    eprintln!(
        "prints mode={mode} fork={fork} checkpoint=APP_LISTENING emitted={:?}",
        process
            .get_stdout()
            .await
            .lines()
            .find(|line| *line == "APP_LISTENING")
    );
    proxy
        .send(DaemonMessage::LocalShare(LocalShareEvent::Ready {
            url: "https://test.example".to_owned(),
        }))
        .await;
    if json {
        process
            .wait_for_line_stdout(Duration::from_secs(5), "https://test.example")
            .await;
    } else {
        process
            .wait_for_line(Duration::from_secs(5), "https://test.example")
            .await;
    }
    if fork {
        process
            .child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"fork\n")
            .await
            .unwrap();
        process
            .wait_for_line_stdout(Duration::from_secs(5), "CHILD_REGISTERED")
            .await;
        eprintln!(
            "prints mode={mode} fork={fork} checkpoint=CHILD_REGISTERED emitted={:?}",
            process
                .get_stdout()
                .await
                .lines()
                .find(|line| *line == "CHILD_REGISTERED")
        );
        process
            .child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"close\n")
            .await
            .unwrap();
        process
            .wait_for_line_stdout(Duration::from_secs(5), "PARENT_CLOSED")
            .await;
        eprintln!(
            "prints mode={mode} fork={fork} checkpoint=PARENT_CLOSED emitted={:?}",
            process
                .get_stdout()
                .await
                .lines()
                .find(|line| *line == "PARENT_CLOSED")
        );
        // The child still listens on the entry port, so the intproxy keeps the steal
        // subscription, which keeps the link.
        let after_parent_closed = timeout(Duration::from_secs(1), proxy.try_recv()).await;
        assert!(
            after_parent_closed.is_err(),
            "prints mode={mode} fork={fork} stage=parent-closed: actual={after_parent_closed:?}, stdout={:.8192}, stderr={:.8192}",
            process.get_stdout().await,
            process.get_stderr().await
        );
    }
    proxy
        .send(DaemonMessage::LocalShare(LocalShareEvent::Failed {
            message: "share registration failed".to_owned(),
        }))
        .await;
    let status = timeout(Duration::from_secs(5), process.wait()).await;
    assert!(
        matches!(status, Ok(status) if status.success().not()),
        "prints mode={mode} fork={fork} stage=application-exit: actual={status:?}, stdout={:.8192}, stderr={:.8192}",
        process.get_stdout().await,
        process.get_stderr().await
    );
    let (reports, other) = if json {
        (process.get_stdout().await, process.get_stderr().await)
    } else {
        (process.get_stderr().await, process.get_stdout().await)
    };
    for (report, count) in [
        (waiting.as_str(), 1),
        ("https://test.example", 1),
        (
            "Note: people who open this link reach the service running on your machine until you stop this session. Who can open it is set by your cluster's access rules.",
            1,
        ),
        ("Can't share this session: share registration failed", 1),
    ] {
        assert_eq!(
            reports.matches(report).count(),
            count,
            "{report}: {reports}"
        );
        assert!(other.contains(report).not(), "{report}: {other}");
    }
    let link_line = reports
        .lines()
        .find(|line| line.contains("https://test.example"))
        .unwrap();
    if json {
        for line in reports.lines().filter(|line| {
            line.contains(&waiting)
                || line.contains("https://test.example")
                || line.contains("Can't share")
        }) {
            let message: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{error}: {line:?}, stdout={reports}"));
            assert_eq!(
                message.get("type").and_then(serde_json::Value::as_str),
                Some("Info"),
                "{reports}"
            );
        }
        let message: serde_json::Value = serde_json::from_str(link_line).unwrap();
        assert!(
            message
                .get("message")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|text| text.starts_with("https://test.example\n")),
            "{reports}"
        );
    } else {
        assert_eq!(link_line, "https://test.example", "{reports}");
    }
    let completion = timeout(Duration::from_secs(5), proxy.wait()).await;
    assert!(
        matches!(completion, Ok(Err(_))),
        "prints mode={mode} fork={fork} stage=proxy-exit: actual={completion:?}, stdout={:.8192}, stderr={:.8192}",
        process.get_stdout().await,
        process.get_stderr().await
    );
    let reusable = TcpListener::bind(("127.0.0.1", local_port))
        .expect("fatal sharing failure left the application listener alive");
    drop(reusable);
}

/// The first listed port, which the link opens, survives `verify-config --resolved`, saving its
/// output as a config file and loading it again, and the internal handoff to the layer.
#[tokio::test]
async fn local_share_resolved_config_keeps_first_port() {
    let scenario = tempfile::TempDir::new().unwrap();
    let tmpdir = scenario.path().to_str().unwrap();
    let mut file = tempfile::NamedTempFile::with_suffix(".toml").unwrap();
    file.write_all(
        b"target = 'pod/mock-target'\nshare = true\n[feature.network.incoming]\nmode = 'steal'\nports = [9000, 8080]\n",
    )
    .unwrap();
    let mut context = ConfigContext::default().strict_env(true);
    let config = LayerFileConfig::from_path(file.path(), &mut context)
        .unwrap()
        .generate_config(&mut context)
        .unwrap();
    assert!(config.share);
    assert_eq!(
        config.feature.network.incoming.first_configured_port,
        Some(9000)
    );
    let output =
        mirrord_command::resolve_tokio_command(std::env::var("MIRRORD_TESTS_USE_BINARY").unwrap())
            .env("TMPDIR", tmpdir)
            .args(["verify-config", "--resolved"])
            .arg(file.path())
            .output()
            .await
            .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let verified: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        verified.get("type").and_then(serde_json::Value::as_str),
        Some("Success"),
        "{verified}"
    );
    let resolved = verified.get("resolved_config").unwrap();
    assert_eq!(
        resolved.pointer("/feature/network/incoming/ports"),
        Some(&serde_json::json!([9000, 8080])),
        "{verified}"
    );
    let mut saved = tempfile::NamedTempFile::with_suffix(".json").unwrap();
    serde_json::to_writer(&mut saved, resolved).unwrap();
    let reloaded = LayerFileConfig::from_path(saved.path(), &mut context)
        .unwrap()
        .generate_config(&mut context)
        .unwrap();
    assert!(reloaded.share);
    assert_eq!(
        reloaded.feature.network.incoming.first_configured_port,
        Some(9000)
    );
    assert_eq!(
        reloaded.feature.network.incoming.ports,
        Some([9000, 8080].into_iter().collect())
    );
    // Internal handoffs must retain generated metadata rather than project file settings.
    let handed_off = LayerConfig::decode(&reloaded.encode().unwrap()).unwrap();
    assert_eq!(
        handed_off.feature.network.incoming.first_configured_port,
        Some(9000)
    );
    assert_eq!(handed_off.key, reloaded.key);
}

/// `mirrord exec --share` refuses a config that a shared session can't use before it starts
/// anything, and prints the reason as it is, without the generic config error around it.
#[rstest::rstest]
#[case::mirror(
    "share = false\n[feature.network]\nincoming = 'mirror'\n",
    "Can't share this session yet: share links need incoming mode 'steal'."
)]
#[case::copy_target(
    "[feature]\ncopy_target = true\n[feature.network]\nincoming = 'steal'\n",
    "Share links aren't supported with copy_target yet."
)]
#[tokio::test]
async fn local_share_exec_rejects_unsupported_config(
    #[case] contents: &str,
    #[case] message: &str,
) {
    let scenario = tempfile::TempDir::new().unwrap();
    let tmpdir = scenario.path().to_str().unwrap();
    let mut config = tempfile::NamedTempFile::with_suffix(".toml").unwrap();
    config.write_all(contents.as_bytes()).unwrap();
    let output =
        mirrord_command::resolve_tokio_command(std::env::var("MIRRORD_TESTS_USE_BINARY").unwrap())
            .env("TMPDIR", tmpdir)
            .args([
                "exec",
                "--share",
                "--disable-version-check",
                "--config-file",
            ])
            .arg(config.path())
            .args(["--", "true"])
            .output()
            .await
            .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success().not(), "{stderr}");
    assert!(stderr.contains(message), "{stderr}");
    assert!(
        stderr.contains("Failed to verify mirrord config").not()
            && stderr.contains("Conflicting configuration").not(),
        "{stderr}"
    );
}

/// A full stderr pipe, which blocks printing the link, must not stop a share failure from ending
/// the application.
#[tokio::test]
async fn local_share_blocked_stderr_does_not_block_failure() {
    let scenario = tempfile::TempDir::new().unwrap();
    let tmpdir = scenario.path().to_str().unwrap();
    let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserved.local_addr().unwrap().port();
    drop(reserved);
    let mut config = tempfile::NamedTempFile::with_suffix(".toml").unwrap();
    write!(config, "share = true\n[feature]\nenv = false\nfs = 'local'\n[feature.network]\noutgoing = false\ndns = false\n[feature.network.incoming]\nmode = 'steal'\nports = [{port}]\n").unwrap();
    let listener = AsyncTcpListener::bind("127.0.0.1:0").await.unwrap();
    let env = get_env(
        listener.local_addr().unwrap(),
        vec![("MIRRORD_PROGRESS_MODE", "off"), ("TMPDIR", tmpdir)],
    );
    let script = format!(
        "import os,socket\ns=socket.socket()\ns.bind(('127.0.0.1',{port}))\ns.listen()\nwhile True: os.write(2,b'x')"
    );
    let mut child =
        mirrord_command::resolve_tokio_command(std::env::var("MIRRORD_TESTS_USE_BINARY").unwrap())
            .args(["exec", "--disable-version-check", "--config-file"])
            .arg(config.path())
            .args(["--", "python3", "-c", &script])
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    // SAFETY: the descriptor belongs to this test; F_GETPIPE_SZ reads kernel pipe metadata.
    let capacity = unsafe { libc::fcntl(stderr.as_raw_fd(), libc::F_GETPIPE_SZ) };
    assert!(capacity > 0);
    let mut proxy = TestIntProxy::new(listener, Some(config.path())).await;
    let start = timeout(Duration::from_secs(15), proxy.try_recv()).await;
    assert!(
        matches!(
            start,
            Ok(Some(ClientMessage::LocalShare(
                LocalShareClientMessage::Start
            )))
        ),
        "blocked mode=off fork=false stage=Start: actual={start:?}, stdout={:.8192}, stderr={:.8192}",
        {
            eprintln!("blocked failure observation start={:?}", start);
            queued_output(&mut stdout).await
        },
        queued_output(&mut stderr).await
    );
    proxy
        .send(DaemonMessage::LocalShare(LocalShareEvent::Accepted {
            entry_port: port,
        }))
        .await;
    let subscribe = timeout(Duration::from_secs(5), proxy.try_recv()).await;
    assert!(
        matches!(
            subscribe,
            Ok(Some(ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(
                _
            ))))
        ),
        "blocked mode=off fork=false stage=subscription: actual={subscribe:?}, stdout={:.8192}, stderr={:.8192}",
        {
            eprintln!("blocked failure observation subscribe={:?}", subscribe);
            queued_output(&mut stdout).await
        },
        queued_output(&mut stderr).await
    );
    proxy
        .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(
            port,
        ))))
        .await;
    let full = timeout(Duration::from_secs(5), async {
        loop {
            let mut queued: libc::c_int = 0;
            // SAFETY: FIONREAD writes one c_int to the valid local pointer for this owned fd.
            assert_eq!(
                unsafe { libc::ioctl(stderr.as_raw_fd(), libc::FIONREAD, &mut queued) },
                0
            );
            if queued == capacity {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        full.is_ok(),
        "blocked mode=off fork=false stage=pipe-full actual={full:?}, stdout={:.8192}, stderr={:.8192}",
        {
            eprintln!("blocked failure observation full={:?}", full);
            queued_output(&mut stdout).await
        },
        queued_output(&mut stderr).await
    );
    proxy
        .send(DaemonMessage::LocalShare(LocalShareEvent::Ready {
            url: "https://blocked.example".to_owned(),
        }))
        .await;
    proxy
        .send(DaemonMessage::LocalShare(LocalShareEvent::Failed {
            message: "forced blocked-output failure".to_owned(),
        }))
        .await;
    let status = timeout(Duration::from_secs(5), child.wait()).await;
    assert!(
        matches!(status, Ok(Ok(status)) if status.success().not()),
        "blocked mode=off fork=false stage=application-exit: actual={status:?}, stdout={:.8192}, stderr={:.8192}",
        {
            eprintln!("blocked failure observation status={:?}", status);
            queued_output(&mut stdout).await
        },
        queued_output(&mut stderr).await
    );
    let completion = timeout(Duration::from_secs(5), proxy.wait()).await;
    assert!(
        matches!(completion, Ok(Err(_))),
        "blocked mode=off fork=false stage=proxy-exit: actual={completion:?}, stdout={:.8192}, stderr={:.8192}",
        {
            eprintln!("blocked failure observation completion={:?}", completion);
            queued_output(&mut stdout).await
        },
        queued_output(&mut stderr).await
    );
    drop(stderr);
    drop(
        TcpListener::bind(("127.0.0.1", port))
            .expect("blocked reporting left the application listener alive"),
    );
}

/// The intproxy of a shared session rejects a port subscription from a layer whose share control
/// connection isn't open, and ends the session, because that application would listen on the
/// shared port while the intproxy has no way to print the link from it. The layer here runs
/// without `share`, so it registers and starts the application, but never opens the control
/// connection.
#[tokio::test]
async fn local_share_rejects_subscription_without_control() {
    let scenario = tempfile::TempDir::new().unwrap();
    let tmpdir = scenario.path().to_str().unwrap();
    let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserved.local_addr().unwrap().port();
    drop(reserved);
    let mut shared = tempfile::NamedTempFile::with_suffix(".toml").unwrap();
    write!(
        shared,
        "share = true\n[feature.network.incoming]\nmode = 'steal'\nports = [{port}]\n"
    )
    .unwrap();
    let mut non_sharing = tempfile::NamedTempFile::with_suffix(".toml").unwrap();
    write!(non_sharing, "share = false\n[feature]\nenv = false\nfs = 'local'\n[feature.network]\noutgoing = false\ndns = false\n[feature.network.incoming]\nmode = 'steal'\nports = [{port}]\n").unwrap();
    let listener = AsyncTcpListener::bind("127.0.0.1:0").await.unwrap();
    let env = get_env(listener.local_addr().unwrap(), vec![("TMPDIR", tmpdir)]);
    let app = Application::DynamicApp(
        "python3".to_owned(),
        vec![
            "-c".to_owned(),
            format!(
                "import socket,sys\nprint('APPLICATION_ENTRY_SENTINEL',flush=True)\nsys.stdin.readline()\ns=socket.socket()\ns.bind(('127.0.0.1',{port}))\ns.listen()\ns.accept()"
            ),
        ],
    );
    let mut process = app.get_test_process(env, Some(non_sharing.path())).await;
    let mut proxy = TestIntProxy::new(listener, Some(shared.path())).await;
    let start = timeout(Duration::from_secs(15), proxy.try_recv()).await;
    assert!(
        matches!(
            start,
            Ok(Some(ClientMessage::LocalShare(
                LocalShareClientMessage::Start
            )))
        ),
        "missing-control mode=default fork=false stage=Start: actual={start:?}, stdout={:.8192}, stderr={:.8192}",
        process.get_stdout().await,
        process.get_stderr().await
    );
    process
        .wait_for_line_stdout(Duration::from_secs(5), "APPLICATION_ENTRY_SENTINEL")
        .await;
    assert!(
        process
            .get_stdout()
            .await
            .lines()
            .any(|line| line == "APPLICATION_ENTRY_SENTINEL")
    );
    proxy
        .send(DaemonMessage::LocalShare(LocalShareEvent::Accepted {
            entry_port: port,
        }))
        .await;
    process
        .child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"listen\n")
        .await
        .unwrap();
    let status = timeout(Duration::from_secs(5), process.wait()).await;
    assert!(
        matches!(status, Ok(status) if status.success().not()),
        "missing-control mode=default fork=false stage=application-exit: actual={status:?}, stdout={:.8192}, stderr={:.8192}",
        process.get_stdout().await,
        process.get_stderr().await
    );
    assert!(
        process
            .get_stdout()
            .await
            .contains("https://missing-control.example")
            .not()
    );
    assert!(
        process
            .get_stderr()
            .await
            .contains("https://missing-control.example")
            .not()
    );
    let completion = timeout(Duration::from_secs(5), proxy.wait()).await;
    assert!(
        matches!(&completion, Ok(Err(error)) if error.to_string().contains("The application subscribed to a port before its share connection was attached.")),
        "missing-control mode=default fork=false stage=proxy-exit: actual={completion:?}, stdout={:.8192}, stderr={:.8192}",
        process.get_stdout().await,
        process.get_stderr().await
    );
    drop(
        TcpListener::bind(("127.0.0.1", port))
            .expect("missing-control failure left the application listener alive"),
    );
}

/// An application that stops listening on the port that the link opens is told that the link
/// ended, because the operator removes the link then while the session runs on. Its natural
/// completion then ends the session cleanly: the intproxy exits with success, and the operator
/// ends the link with the session's connection.
#[tokio::test]
async fn local_share_normal_application_exit_is_clean() {
    let scenario = tempfile::TempDir::new().unwrap();
    let tmpdir = scenario.path().to_str().unwrap();
    let reserved = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reserved.local_addr().unwrap().port();
    drop(reserved);
    let mut config = tempfile::NamedTempFile::with_suffix(".toml").unwrap();
    write!(config, "share = true\n[feature]\nenv = false\nfs = 'local'\n[feature.network]\noutgoing = false\ndns = false\n[feature.network.incoming]\nmode = 'steal'\nports = [{port}]\n").unwrap();
    let app = Application::DynamicApp(
        "python3".to_owned(),
        vec![
            "-c".to_owned(),
            format!(
                "import os,socket,sys\ns=socket.socket()\ns.bind(('127.0.0.1',{port}))\ns.listen()\nprint('APP_'+'LISTENING',flush=True)\nsys.stdin.readline()\ns.close()\nprint('APP_'+'CLOSED',flush=True)\nsys.stdin.readline()\nos._exit(0)"
            ),
        ],
    );
    let (mut process, mut proxy) = app
        .start_process(vec![("TMPDIR", tmpdir)], Some(config.path()))
        .await;
    let start = timeout(Duration::from_secs(15), proxy.try_recv()).await;
    assert!(
        matches!(
            start,
            Ok(Some(ClientMessage::LocalShare(
                LocalShareClientMessage::Start
            )))
        ),
        "normal mode=default fork=false stage=Start: actual={start:?}, stdout={:.8192}, stderr={:.8192}",
        process.get_stdout().await,
        process.get_stderr().await
    );
    proxy
        .send(DaemonMessage::LocalShare(LocalShareEvent::Accepted {
            entry_port: port,
        }))
        .await;
    let subscription = timeout(Duration::from_secs(5), proxy.try_recv()).await;
    assert!(
        matches!(
            subscription,
            Ok(Some(ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(
                _
            ))))
        ),
        "normal mode=default fork=false stage=subscription: actual={subscription:?}, stdout={:.8192}, stderr={:.8192}",
        process.get_stdout().await,
        process.get_stderr().await
    );
    proxy
        .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(
            port,
        ))))
        .await;
    process
        .wait_for_line_stdout(Duration::from_secs(5), "APP_LISTENING")
        .await;
    eprintln!(
        "normal mode=default fork=false checkpoint=APP_LISTENING emitted={:?}",
        process
            .get_stdout()
            .await
            .lines()
            .find(|line| *line == "APP_LISTENING")
    );
    proxy
        .send(DaemonMessage::LocalShare(LocalShareEvent::Ready {
            url: "https://normal.example".to_owned(),
        }))
        .await;
    process
        .wait_for_line(Duration::from_secs(5), "https://normal.example")
        .await;
    process
        .child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"close\n")
        .await
        .unwrap();
    let unsubscribe = timeout(Duration::from_secs(5), proxy.try_recv()).await;
    assert!(
        matches!(
            unsubscribe,
            Ok(Some(ClientMessage::TcpSteal(LayerTcpSteal::PortUnsubscribe(unsubscribed)))) if unsubscribed == port
        ),
        "normal mode=default fork=false stage=listener-closed: actual={unsubscribe:?}, stdout={:.8192}, stderr={:.8192}",
        process.get_stdout().await,
        process.get_stderr().await
    );
    let ended = format!(
        "The share link ended because your application stopped listening on port {port}. Restart the session to get a new link."
    );
    process.wait_for_line(Duration::from_secs(5), &ended).await;
    process
        .child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"exit\n")
        .await
        .unwrap();
    let status = timeout(Duration::from_secs(5), process.wait()).await;
    assert!(
        matches!(status, Ok(status) if status.success()),
        "normal mode=default fork=false stage=application-exit: actual={status:?}, stdout={:.8192}, stderr={:.8192}",
        process.get_stdout().await,
        process.get_stderr().await
    );
    // The intproxy closes the connection once its idle timeout passes after the application's
    // exit, and sends nothing for the share link before that.
    loop {
        let message = timeout(Duration::from_secs(10), proxy.try_recv()).await;
        match message {
            Ok(None) => break,
            Ok(Some(ClientMessage::TcpSteal(LayerTcpSteal::PortUnsubscribe(_)))) => {}
            other => panic!(
                "normal mode=default fork=false stage=close actual={other:?}, stdout={:.8192}, stderr={:.8192}",
                process.get_stdout().await,
                process.get_stderr().await
            ),
        }
    }
    let completion = timeout(Duration::from_secs(10), proxy.wait()).await;
    assert!(
        matches!(completion, Ok(Ok(_))),
        "normal mode=default fork=false stage=proxy-exit: actual={completion:?}, stdout={:.8192}, stderr={:.8192}",
        process.get_stdout().await,
        process.get_stderr().await
    );
    let stderr = process.get_stderr().await;
    for report in [
        format!("Waiting for your application to listen on port {port} to share it."),
        "https://normal.example".to_owned(),
        ended,
    ] {
        assert_eq!(stderr.matches(&report).count(), 1, "{report}: {stderr}");
    }
}
