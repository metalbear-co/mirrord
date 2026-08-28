//! Runs the real binary on a pty and reads back what it renders, which is the only way to check the
//! parts of the terminal screen that need a terminal: layout, the pane-derived shell size, and the
//! `C-b` prefix.
//!
//! Unix-only: the pty these drive and the session sockets they serve are both unix concepts, and
//! clippy runs `--all-targets` against the Windows target.
#![cfg(unix)]

use std::{
    io::{Read, Write},
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use mirrord_session_monitor_client::{ProcessInfo, SessionInfo};
use mirrord_session_monitor_protocol::PortSubscription;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

const ROWS: u16 = 30;
const COLS: u16 = 100;
const TIMEOUT: Duration = Duration::from_secs(30);

/// Feeds the captured stream into an emulator until the rendered screen contains `needle`.
fn wait_for(rx: &Receiver<Vec<u8>>, parser: &mut vt100::Parser, needle: &str) -> String {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let contents = parser.screen().contents();
        if contents.contains(needle) {
            return contents;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(bytes) => parser.process(&bytes),
            Err(RecvTimeoutError::Timeout) => {
                panic!("timed out waiting for {needle:?}; screen was:\n{contents}")
            }
            Err(RecvTimeoutError::Disconnected) => {
                panic!("the tui exited before {needle:?} appeared; screen was:\n{contents}")
            }
        }
    }
}

/// A private `HOME` for one test: the session registry the panel reads lives under it, so this is
/// what keeps a developer's own mirrord sessions (and shell rc files) out of the assertions.
///
/// The name is kept short on purpose — the session socket's path sits under this one, and a unix
/// socket path has to fit in `SUN_LEN` (104 bytes on macOS, where the temp directory alone is 48).
fn temporary_home(name: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("mt-{name}-{}", std::process::id()));

    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".mirrord").join("sessions")).unwrap();

    home
}

/// Serves `info` over a session socket the way mirrord's internal proxy does, for as long as the
/// test runs.
fn serve_session(home: &Path, session_id: &str, info: SessionInfo) {
    let socket = home
        .join(".mirrord")
        .join("sessions")
        .join(format!("{session_id}.sock"));
    let listener = UnixListener::bind(socket).unwrap();
    let body = serde_json::to_vec(&info).unwrap();

    thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let body = body.clone();
            thread::spawn(move || {
                // The client sends a bodyless GET, which arrives in one read.
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);

                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                    body.len(),
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            });
        }
    });
}

/// Resizes the pty the tui runs on, keeping the emulator that reads it back in agreement.
fn resize(master: &dyn MasterPty, parser: &mut vt100::Parser, rows: u16, cols: u16) {
    master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    parser.screen_mut().set_size(rows, cols);
}

#[test]
fn shell_is_constrained_to_the_pane_and_the_prefix_takes_the_keyboard_back() {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mirrord-tui"));
    cmd.env("TERM", "xterm-256color");
    // A login shell with the developer's rc files would make the assertions non-deterministic.
    cmd.env("SHELL", "/bin/sh");
    cmd.env("PS1", "$ ");
    cmd.env("HOME", temporary_home("none"));
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                return;
            }
        }
    });

    let mut parser = vt100::Parser::new(ROWS, COLS, 0);
    let mut writer = pair.master.take_writer().unwrap();

    // `Shift+Tab` from the first screen wraps around to the terminal, which is the last one.
    wait_for(&rx, &mut parser, "Welcome!");
    writer.write_all(b"\x1b[Z").unwrap();
    writer.flush().unwrap();

    // The border is drawn, and its title advertises the size handed to the shell: the body area
    // (the frame minus the tabs and status lines) minus the border. Each `wait_for` keeps reading
    // until the screen settles, so a half-painted first frame is not a failure.
    // Only the size is asserted on, so that rewording the key hint beside it is not a test change.
    let screen = wait_for(&rx, &mut parser, "shell 98x24 \u{2014}");
    wait_for(&rx, &mut parser, "╯");

    // This shell has no mirrord session under it, so the session panel takes none of its columns —
    // which is exactly why the shell above is 98 wide and not 64.
    assert!(
        !screen.contains("╭ mirrord "),
        "the session panel should not be drawn without sessions; screen was:\n{screen}",
    );

    // The shell's own view has to agree: the pane's inner rectangle, not the 30x100 outer pty.
    writer.write_all(b"stty size\r").unwrap();
    writer.flush().unwrap();
    wait_for(&rx, &mut parser, "24 98");

    // Shrinking the window has to reach the shell, not just the border: the pane re-derives its
    // size, resizes the pty, and the shell reflows. The panel keeps its width — the shell is what
    // gives up the columns.
    resize(&*pair.master, &mut parser, 24, 80);
    wait_for(&rx, &mut parser, "shell 78x18");

    writer.write_all(b"stty size\r").unwrap();
    writer.flush().unwrap();
    wait_for(&rx, &mut parser, "18 78");

    // `q` belongs to the shell while it has the keyboard, so it has to reach the shell as a
    // keystroke rather than quitting the application.
    writer.write_all(b"echo q").unwrap();
    writer.flush().unwrap();
    wait_for(&rx, &mut parser, "$ echo q");

    // C-b is held back from the shell, and only then is `q` taken as the global quit key.
    writer.write_all(&[0x02]).unwrap();
    writer.flush().unwrap();
    wait_for(&rx, &mut parser, "⟨C-b⟩");

    writer.write_all(b"q").unwrap();
    writer.flush().unwrap();

    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "tui exited with {}", status.exit_code());
                break;
            }
            None if Instant::now() > deadline => panic!("`C-b q` did not quit the tui"),
            None => thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// The other half of the split: a session started from the pane's shell brings the panel in, and
/// the shell gives up the columns for it.
#[test]
fn a_session_under_the_shell_brings_the_panel_in() {
    let home = temporary_home("one");

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mirrord-tui"));
    cmd.env("TERM", "xterm-256color");
    cmd.env("SHELL", "/bin/sh");
    cmd.env("PS1", "$ ");
    cmd.env("HOME", &home);
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                return;
            }
        }
    });

    let mut parser = vt100::Parser::new(ROWS, COLS, 0);
    let mut writer = pair.master.take_writer().unwrap();

    wait_for(&rx, &mut parser, "Welcome!");
    writer.write_all(b"\x1b[Z").unwrap();
    writer.flush().unwrap();

    // No sessions yet, so the shell has the whole body.
    wait_for(&rx, &mut parser, "shell 98x24 \u{2014}");

    // The panel attributes sessions by process tree, so the session has to name a process that
    // really is under this shell — and the shell itself is the one process the test can name.
    // `$$` is written split so that the command's own echo cannot be mistaken for its output.
    writer.write_all(b"echo SHELL''PID=$$\r").unwrap();
    writer.flush().unwrap();
    let screen = wait_for(&rx, &mut parser, "SHELLPID=");
    let shell_pid: u32 = screen
        .split("SHELLPID=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("could not read the shell's pid; screen was:\n{screen}"));

    serve_session(
        &home,
        "fake",
        SessionInfo {
            session_id: "fake".to_owned(),
            key: None,
            target: "deployment/api".to_owned(),
            namespace: Some("staging".to_owned()),
            context: None,
            started_at: k8s_openapi::jiff::Timestamp::now().to_string(),
            mirrord_version: "3.249.0".to_owned(),
            is_operator: true,
            processes: vec![ProcessInfo {
                pid: shell_pid,
                parent_pid: None,
                process_name: "node".to_owned(),
                cmdline: vec!["node".to_owned(), "server.js".to_owned()],
            }],
            port_subscriptions: vec![PortSubscription {
                port: 8080,
                mode: "steal".to_owned(),
            }],
            config: serde_json::Value::Null,
        },
    );

    // The next poll finds it: the panel appears with the session in it, and the shell narrows by
    // exactly the panel's width.
    wait_for(&rx, &mut parser, "╭ mirrord ");
    wait_for(&rx, &mut parser, "deployment/api");
    wait_for(&rx, &mut parser, "steal :8080");
    wait_for(&rx, &mut parser, "shell 64x24 \u{2014}");

    // ...and the shell is told about its new size, not just drawn smaller.
    writer.write_all(b"stty size\r").unwrap();
    writer.flush().unwrap();
    wait_for(&rx, &mut parser, "24 64");

    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}

/// Writes a kubeconfig whose credential plugin fails the way `gke-gcloud-auth-plugin` does with an
/// expired login: it complains on stderr and exits non-zero. `kube` runs it with our own stderr
/// inherited (nothing here sets `interactiveMode: Never`, which is the only way to opt out), so
/// this is what puts another process's output on the terminal the interface is drawing on.
fn failing_kubeconfig(home: &Path) -> PathBuf {
    let path = home.join("kubeconfig");

    std::fs::write(
        &path,
        format!(
            "apiVersion: v1
kind: Config
current-context: fake
clusters:
- name: fake
  cluster:
    server: https://127.0.0.1:1
contexts:
- name: fake
  context:
    cluster: fake
    user: fake
users:
- name: fake
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1beta1
      command: /bin/sh
      args:
      - -c
      - >-
        echo '{KLOG}' >&2; exit 1
"
        ),
    )
    .unwrap();

    path
}

/// The one line the fake plugin prints. Long, unpunctuated by carriage returns, and ending in the
/// instruction that is the only actionable part of the whole failure - like the real thing.
const KLOG: &str = "F0826 10:31:14.086748 21795 cred.go:150] print credential failed with error: \
                    Reauthentication failed. Please run: gcloud auth login";

#[test]
fn a_failing_credential_plugin_stays_out_of_the_interface() {
    let home = temporary_home("auth");
    let kubeconfig = failing_kubeconfig(&home);

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mirrord-tui"));
    cmd.env("TERM", "xterm-256color");
    cmd.env("SHELL", "/bin/sh");
    cmd.env("PS1", "$ ");
    cmd.env("HOME", &home);
    cmd.env("KUBECONFIG", &kubeconfig);
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                return;
            }
        }
    });

    let mut parser = vt100::Parser::new(ROWS, COLS, 0);
    let mut writer = pair.master.take_writer().unwrap();

    // The status bar reports the failure...
    let screen = wait_for(&rx, &mut parser, "\u{2717} ");

    // ...and that is all it reports: the plugin's own output went to the log, and the summary of
    // the error stopped where its payload starts, rather than spilling the quoted command (and the
    // `KUBERNETES_EXEC_INFO` JSON in it) across the rest of the screen.
    assert!(
        !screen.contains("cred.go"),
        "the plugin's stderr should not reach the terminal; screen was:\n{screen}",
    );
    assert!(
        !screen.contains("KUBERNETES_EXEC_INFO"),
        "the status bar should not spill the exec payload; screen was:\n{screen}",
    );
    assert!(
        screen.contains("Welcome!"),
        "the home screen should still be intact; screen was:\n{screen}",
    );

    // The summary leaves room for the hint that says where the rest of it went.
    wait_for(&rx, &mut parser, "e for details");

    // `e` is where the detail went, and the plugin's own words are in it - they are the ones that
    // say what to do about the failure.
    writer.write_all(b"e").unwrap();
    writer.flush().unwrap();
    wait_for(&rx, &mut parser, "connection error");
    wait_for(&rx, &mut parser, "gcloud auth login");
    wait_for(&rx, &mut parser, "esc to close");

    // Esc closes it again.
    writer.write_all(b"\x1b").unwrap();
    writer.flush().unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let screen = parser.screen().contents();
        if !screen.contains("connection error") {
            break;
        }
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(bytes) => parser.process(&bytes),
            Err(_) => panic!("esc did not close the dialog; screen was:\n{screen}"),
        }
    }

    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&home);
}
