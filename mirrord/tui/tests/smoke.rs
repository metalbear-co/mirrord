//! Runs the real binary on a pty and reads back what it renders, which is the only way to check the
//! parts of the terminal screen that need a terminal: layout, the pane-derived shell size, and the
//! `C-b` prefix.
//!
//! Unix-only: the pty these drive and the session sockets they serve are both unix concepts, and
//! clippy runs `--all-targets` against the Windows target.
#![cfg(unix)]
// Every slice here is `buf[..n]` straight out of a `read` that just reported `n` bytes into it.
#![allow(clippy::indexing_slicing)]

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

/// Feeds the captured stream into an emulator until `needle` is gone from the rendered screen.
///
/// Needed after `Esc`: a key sent right behind it would arrive as one `Alt+<key>` press, the way a
/// terminal encodes Meta, so the test waits for the `Esc` to land first.
fn wait_for_gone(rx: &Receiver<Vec<u8>>, parser: &mut vt100::Parser, needle: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let contents = parser.screen().contents();
        if !contents.contains(needle) {
            return;
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(bytes) => parser.process(&bytes),
            Err(error) => panic!("{needle:?} never went away ({error}); screen was:\n{contents}"),
        }
    }
}

/// An empty folder to start the TUI in: no project files, so it opens on the dashboard rather
/// than on the Run view.
fn empty_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mt-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
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

/// Where `needle` first appears on a screen dump from [`wait_for`]: its row and column, counted
/// from zero, for clicking on it.
fn locate(screen: &str, needle: &str) -> (usize, usize) {
    screen
        .lines()
        .enumerate()
        .find_map(|(row, line)| {
            line.find(needle)
                .map(|byte| (row, line[..byte].chars().count()))
        })
        .unwrap_or_else(|| panic!("{needle:?} is not on the screen:\n{screen}"))
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
    // Outside any project, so the TUI opens on the dashboard; tests that need a project set their
    // own.
    cmd.cwd(empty_dir());
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

    // Outside a project the TUI opens on the dashboard; `:terminal` goes to the shell.
    wait_for(&rx, &mut parser, "\u{256d} home");
    writer.write_all(b":terminal\r").unwrap();
    writer.flush().unwrap();

    // The border is drawn, and its title advertises the size handed to the shell: the body area
    // (the frame minus the top bar and the key bar) minus the border. Each `wait_for` keeps reading
    // until the screen settles, so a half-painted first frame is not a failure.
    // Only the size is asserted on, so that rewording the key hint beside it is not a test change.
    let screen = wait_for(&rx, &mut parser, "shell 76x27 \u{2014}");
    wait_for(&rx, &mut parser, "╯");

    // This shell has no mirrord session under it, so the session panel takes none of its columns —
    // which is exactly why the shell above is 98 wide and not 64.
    assert!(
        !screen.contains("╭ mirrord "),
        "the session panel should not be drawn without sessions; screen was:\n{screen}",
    );

    // A prompt long enough to fill the pane wraps what is typed after it onto the next line, and a
    // wrapped line matches no needle that spans the break. `PS1` in the environment does not settle
    // that: the shell is started as a login shell, and where `/bin/sh` is bash its system rc files
    // replace the prompt with one built from the hostname - long enough on a CI runner to wrap a
    // six-character command on its own. Asking for the prompt from inside the shell does settle it,
    // because nothing runs afterwards to override it. Written split so that the command's own echo
    // cannot be mistaken for the prompt it asks for.
    writer.write_all(b"PS1=mt'> '\r").unwrap();
    writer.flush().unwrap();
    wait_for(&rx, &mut parser, "mt> ");

    // The shell's own view has to agree: the pane's inner rectangle, not the 30x100 outer pty.
    writer.write_all(b"stty size\r").unwrap();
    writer.flush().unwrap();
    wait_for(&rx, &mut parser, "27 76");

    // Shrinking the window has to reach the shell, not just the border: the pane re-derives its
    // size, resizes the pty, and the shell reflows. The panel keeps its width — the shell is what
    // gives up the columns.
    resize(&*pair.master, &mut parser, 24, 80);
    wait_for(&rx, &mut parser, "shell 73x21");

    writer.write_all(b"stty size\r").unwrap();
    writer.flush().unwrap();
    wait_for(&rx, &mut parser, "21 73");

    // `q` belongs to the shell while it has the keyboard, so it has to reach the shell as a
    // keystroke rather than quitting the application. Matched together with the prompt it is typed
    // at, which is what makes it this shell's echo rather than any other `echo q` on the screen.
    writer.write_all(b"echo q").unwrap();
    writer.flush().unwrap();
    wait_for(&rx, &mut parser, "mt> echo q");

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
    // Outside any project, so the TUI opens on the dashboard; tests that need a project set their
    // own.
    cmd.cwd(empty_dir());
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

    wait_for(&rx, &mut parser, "\u{256d} home");
    writer.write_all(b":terminal\r").unwrap();
    writer.flush().unwrap();

    // No sessions yet, so the shell has the whole body.
    wait_for(&rx, &mut parser, "shell 76x27 \u{2014}");

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
                hit_count: None,
            }],
            config: serde_json::Value::Null,
        },
    );

    // The next poll finds it: the panel appears with the session in it, and the shell narrows by
    // exactly the panel's width.
    wait_for(&rx, &mut parser, "╭ mirrord ");
    wait_for(&rx, &mut parser, "deployment/api");
    wait_for(&rx, &mut parser, "steal :8080");
    wait_for(&rx, &mut parser, "shell 42x27 \u{2014}");

    // ...and the shell is told about its new size, not just drawn smaller.
    writer.write_all(b"stty size\r").unwrap();
    writer.flush().unwrap();
    wait_for(&rx, &mut parser, "27 42");

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
    // Outside any project, so the TUI opens on the dashboard; tests that need a project set their
    // own.
    cmd.cwd(empty_dir());
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
        screen.contains(" home "),
        "the dashboard should still be intact; screen was:\n{screen}",
    );

    // The summary leaves room for the hint that says where the rest of it went.
    wait_for(&rx, &mut parser, "<e> details");

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

#[test]
fn help_and_command_palette_reach_every_screen() {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mirrord-tui"));
    // Outside any project, so the TUI opens on the dashboard; tests that need a project set their
    // own.
    cmd.cwd(empty_dir());
    cmd.env("TERM", "xterm-256color");
    cmd.env("HOME", temporary_home("keys"));
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
    let mut send = |bytes: &[u8]| {
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    };

    // The key bar points every screen at the two ways to discover everything else.
    wait_for(&rx, &mut parser, "<?> help");

    // `COLORTERM` is not set here, so the brand colors arrive as named colors: the logo chip's
    // indigo as bright magenta (color 13) rather than a 24-bit value the terminal may not have.
    let logo = parser.screen().cell(1, 2).map(vt100::Cell::bgcolor);
    assert_eq!(logo, Some(vt100::Color::Idx(13)));

    // `?` lists the keys that work everywhere, and closes again on `Esc`.
    send(b"?");
    let screen = wait_for(&rx, &mut parser, "Everywhere");
    println!("{screen}");
    assert!(screen.contains("command palette"), "screen was:\n{screen}");
    send(b"\x1b");
    wait_for_gone(&rx, &mut parser, "Everywhere");

    // `Ctrl+P` opens the palette, typing narrows it, and `Esc` closes it.
    send(&[0x10]);
    wait_for(&rx, &mut parser, "Go to Queues");
    send(b"go to queues");
    wait_for(&rx, &mut parser, "/go to queues");
    send(b"\x1b");
    wait_for_gone(&rx, &mut parser, "/go to queues");
    send(b"\x1b");
    wait_for_gone(&rx, &mut parser, "Go to Queues");

    // `:` takes a resource name, suggesting as it is typed; `Enter` goes to the suggestion.
    send(b":");
    wait_for(&rx, &mut parser, "<tab> complete");
    send(b"qu");
    let screen = wait_for(&rx, &mut parser, "queue splits");
    println!("{screen}");
    send(b"\r");
    wait_for(&rx, &mut parser, "\u{256d} queues \u{2500}");

    // On a screen with its own keys, `?` lists them above the global ones.
    send(b"?");
    let screen = wait_for(&rx, &mut parser, "Everywhere");
    println!("{screen}");
    assert!(screen.contains("details"), "screen was:\n{screen}");
    send(b"\x1b");
    wait_for_gone(&rx, &mut parser, "Everywhere");

    // The shared search box: `/` takes the keyboard, so `q` is typed rather than quitting.
    send(b"/");
    wait_for(&rx, &mut parser, "<type> to filter");
    send(b"q");
    let screen = wait_for(&rx, &mut parser, "/ q");
    println!("{screen}");

    // Sessions is built from the same table, search and footer.
    send(b"\r");
    wait_for(&rx, &mut parser, "<esc> clear");
    send(b"\x1b");
    wait_for_gone(&rx, &mut parser, "/ q");
    send(b":sessions\r");
    wait_for(&rx, &mut parser, "\u{256d} sessions");
    // Read once the frame has finished arriving, rather than asserting on a half-painted one.
    let screen = wait_for(&rx, &mut parser, "<ctrl-d> kill");
    println!("{screen}");
    send(b"/");
    send(b"q");
    wait_for(&rx, &mut parser, "/ q");

    // Previews and targets share the panel style, and `?` on targets is the shared help.
    send(b"\r");
    wait_for(&rx, &mut parser, "<esc> clear");
    send(b"\x1b");
    wait_for_gone(&rx, &mut parser, "/ q");
    send(b":previews\r");
    let screen = wait_for(&rx, &mut parser, "\u{256d} previews");
    println!("{screen}");
    send(b":targets\r");
    let screen = wait_for(&rx, &mut parser, "\u{256d} plan[");
    println!("{screen}");
    send(b"?");
    wait_for(&rx, &mut parser, "Everywhere");
    send(b"\x1b");
    wait_for_gone(&rx, &mut parser, "Everywhere");
    send(b":sessions\r");
    wait_for(&rx, &mut parser, "<ctrl-d> kill");
    send(b"/");
    send(b"q");
    wait_for(&rx, &mut parser, "/ q");

    // `Enter` keeps the phrase and hands the keyboard back, so `Esc` clears it and `q` quits.
    send(b"\r");
    wait_for(&rx, &mut parser, "<esc> clear");
    send(b"\x1b");
    wait_for_gone(&rx, &mut parser, "/ q");
    send(b"q");
    child.wait().unwrap();
}

#[test]
fn config_screen_edits_and_saves_mirrord_json() {
    let home = temporary_home("cfg");
    let project = home.join("project");
    std::fs::create_dir_all(&project).unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mirrord-tui"));
    // Outside any project, so the TUI opens on the dashboard; tests that need a project set their
    // own.
    cmd.cwd(empty_dir());
    cmd.env("TERM", "xterm-256color");
    cmd.env("HOME", &home);
    cmd.cwd(&project);
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
    let mut send = |bytes: &[u8]| {
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    };

    wait_for(&rx, &mut parser, "<?> help");
    send(b":config\r");
    let screen = wait_for(&rx, &mut parser, "\u{256d} config[");
    println!("{screen}");

    // The search opens every section on the way to a match.
    send(b"/");
    send(b"network.incoming");
    wait_for(&rx, &mut parser, "incoming");
    send(b"\r");
    wait_for_gone(&rx, &mut parser, "<type> to filter");
    send(b"jj");
    let screen = wait_for(&rx, &mut parser, "feature.network.incoming ");
    println!("{screen}");

    // `incoming` is unset and takes several forms, so `Enter` asks which one first.
    send(b"\r");
    wait_for(&rx, &mut parser, "Choose a form");
    send(b"steal\r");
    wait_for_gone(&rx, &mut parser, "Choose a form");
    send(b"\r");
    wait_for(&rx, &mut parser, "Choose a value");
    send(b"mirror\r");
    wait_for_gone(&rx, &mut parser, "Choose a value");
    wait_for(&rx, &mut parser, "unsaved");

    // Ctrl+S writes only what was set, then has mirrord check it.
    send(&[0x13]);
    let screen = wait_for(&rx, &mut parser, "saved to .mirrord/mirrord.json");
    println!("{screen}");

    let mirrord_json = project.join(".mirrord").join("mirrord.json");
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&mirrord_json).unwrap()).unwrap();
    assert_eq!(
        saved,
        serde_json::json!({ "feature": { "network": { "incoming": "mirror" } } })
    );

    // mirrord's own checks run on save: stealing needs a target, and this config has none.
    send(b"\r");
    wait_for(&rx, &mut parser, "Choose a value");
    send(b"steal\r");
    wait_for_gone(&rx, &mut parser, "Choose a value");
    send(&[0x13]);
    let screen = wait_for(&rx, &mut parser, "Saved, but mirrord rejects it");
    println!("{screen}");

    // Quitting with unsaved edits asks first: `Esc` stays, `s` saves and quits.
    send(b"d");
    wait_for(&rx, &mut parser, "unsaved");
    send(b"q");
    let screen = wait_for(&rx, &mut parser, "Unsaved changes");
    println!("{screen}");
    assert!(
        screen.contains("edits to .mirrord/mirrord.json are not saved"),
        "screen was:\n{screen}"
    );
    send(b"\x1b");
    wait_for_gone(&rx, &mut parser, "Unsaved changes");
    send(b"q");
    wait_for(&rx, &mut parser, "Unsaved changes");
    send(b"s");
    child.wait().unwrap();

    // Unsetting the only option left the file with nothing in it.
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&mirrord_json).unwrap()).unwrap();
    assert_eq!(saved, serde_json::json!({}));
}

#[test]
fn run_view_works_out_the_project_and_its_command() {
    let home = temporary_home("run");
    let project = home.join("checkout");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("package.json"),
        r#"{ "name": "@shop/checkout", "scripts": { "dev": "node server.js" } }"#,
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mirrord-tui"));
    // Outside any project, so the TUI opens on the dashboard; tests that need a project set their
    // own.
    cmd.cwd(empty_dir());
    cmd.env("TERM", "xterm-256color");
    cmd.env("HOME", &home);
    cmd.cwd(&project);
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
    let mut send = |bytes: &[u8]| {
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    };

    wait_for(&rx, &mut parser, "<?> help");
    // Started inside a project: the folder is already picked, so Run opens on the targets step.
    wait_for(&rx, &mut parser, "What should they run against?");
    wait_for(&rx, &mut parser, "Folders \u{2713} 1");
    // No cluster in the test: Enter goes on without a target.
    send(b"\r");
    wait_for(&rx, &mut parser, "How should mirrord run them?");
    wait_for(&rx, &mut parser, "Start from  \u{2726} No incoming traffic");
    // Every option, the way the Config screen shows them, tiers included.
    wait_for(&rx, &mut parser, "\u{256d} config");
    wait_for(&rx, &mut parser, "new config \u{b7} not saved");
    let screen = wait_for(&rx, &mut parser, " team");
    println!("{screen}");
    wait_for(&rx, &mut parser, "READY   ctrl+r starts it");
    // Ctrl+Y shows exactly what will run: the project's name and dev script, found without asking.
    send(b"\x19");
    wait_for(&rx, &mut parser, "$ npm run dev");
    wait_for(&rx, &mut parser, "detected - check it");
    send(b"\x19");
    wait_for(&rx, &mut parser, "\u{256d} config");

    // Ctrl+S saves the new config: a name, then the folder's .mirrord.
    send(b"\x13");
    wait_for(&rx, &mut parser, "Name the config");
    send(b"with-db\r");
    wait_for(&rx, &mut parser, ".mirrord/with-db.json");
    assert!(project.join(".mirrord").join("with-db.json").exists());
    // Ctrl+O lists it among the configs to start from from now on.
    send(b"\x0f");
    wait_for(&rx, &mut parser, "SAVED IN YOUR FOLDERS");
    let screen = wait_for(&rx, &mut parser, "checkout/.mirrord/with-db.json");
    println!("{screen}");
    send(b"\x1b");
    wait_for_gone(&rx, &mut parser, "SAVED IN YOUR FOLDERS");

    // Esc goes back a step.
    send(b"\x1b");
    wait_for(&rx, &mut parser, "What should they run against?");

    send(b"\x03");
    child.wait().unwrap();
}

#[test]
fn run_view_picks_folders_from_anywhere_and_runs_them_together() {
    let home = temporary_home("pick");
    for (dir, name) in [
        ("shop/checkout", "checkout"),
        ("billing/payments", "payments"),
    ] {
        std::fs::create_dir_all(home.join(dir)).unwrap();
        std::fs::write(
            home.join(dir).join("package.json"),
            format!(r#"{{ "name": "{name}", "scripts": {{ "dev": "node server.js" }} }}"#),
        )
        .unwrap();
    }

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mirrord-tui"));
    // Started outside both projects: where the TUI runs no longer decides what can be run.
    cmd.cwd(empty_dir());
    cmd.env("TERM", "xterm-256color");
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
    let mut send = |bytes: &[u8]| {
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    };

    wait_for(&rx, &mut parser, "<?> help");
    // Shift+Tab steps back a screen, past the Terminal (whose shell would keep the keyboard),
    // and Tab steps forward again.
    send(b"\x1b[Z");
    wait_for(&rx, &mut parser, "\u{258c}\u{25a6} Resources");
    send(b"\t");
    let screen = wait_for(&rx, &mut parser, "\u{258c}\u{2302} Home");
    // A click on a screen in the sidebar goes there.
    let (row, column) = locate(&screen, "Sessions");
    send(
        format!(
            "\x1b[<0;{};{}M\x1b[<0;{};{}m",
            column + 1,
            row + 1,
            column + 1,
            row + 1
        )
        .as_bytes(),
    );
    wait_for(&rx, &mut parser, "\u{258c}\u{25c9} Sessions");
    send(b":run\r");
    wait_for(&rx, &mut parser, "Which folders do you want to run?");

    // Both projects are found under the home folder without typing a path.
    wait_for(&rx, &mut parser, "~/billing/payments");
    let screen = wait_for(&rx, &mut parser, "[ ] ~/shop/checkout");
    println!("{screen}");

    // Typing searches; Space picks, and the pick shows up in the tray with a remove button.
    send(b"shop");
    wait_for_gone(&rx, &mut parser, "~/billing/payments");
    // Cmd+A, as terminals that pass Cmd on send it (kitty keyboard protocol), selects the whole
    // search, and typing replaces it.
    send(b"\x1b[97;9u");
    send(b"pay");
    wait_for_gone(&rx, &mut parser, "~/shop/checkout");
    wait_for(&rx, &mut parser, "\u{2315} pay ");
    // While searching, Enter picks what the search found and clears the search.
    send(b"\r");
    wait_for(&rx, &mut parser, " payments \u{2715}");
    wait_for(&rx, &mut parser, "[ ] ~/shop/checkout");

    // A click on a row picks it too.
    let screen = wait_for(&rx, &mut parser, "[ ] ~/shop/checkout");
    let (row, column) = locate(&screen, "~/shop/checkout");
    send(
        format!(
            "\x1b[<0;{};{}M\x1b[<0;{};{}m",
            column + 1,
            row + 1,
            column + 1,
            row + 1
        )
        .as_bytes(),
    );
    let screen = wait_for(&rx, &mut parser, " checkout \u{2715}");
    println!("{screen}");

    // Enter goes on: no cluster, so there are no targets to pick and Enter goes on again.
    send(b"\r");
    wait_for(&rx, &mut parser, "What should they run against?");
    wait_for(&rx, &mut parser, "Folders \u{2713} 2");
    send(b"\r");
    wait_for(&rx, &mut parser, "How should mirrord run them?");
    let screen = wait_for(&rx, &mut parser, "READY   ctrl+r starts all 2");
    println!("{screen}");

    // Back to the folders, then browse: `~` is the home folder, typing narrows it, right goes
    // into a folder and left comes back out.
    send(b"\x1b");
    wait_for(&rx, &mut parser, "What should they run against?");
    send(b"\x1b");
    wait_for(&rx, &mut parser, "Which folders do you want to run?");
    send(b"~sh");
    wait_for(&rx, &mut parser, "shop/");
    send(b"\x1b[C");
    let screen = wait_for(&rx, &mut parser, "checkout/");
    println!("{screen}");
    send(b"\x1b[D");
    wait_for(&rx, &mut parser, "billing/");
    // Esc goes back to the project list.
    send(b"\x1b");
    wait_for(&rx, &mut parser, "found on this computer");

    send(b"\x03");
    child.wait().unwrap();
}

/// Every editing key, sent as the bytes real terminals send, reaches a text field and does what
/// it does in any text box. The Run view's search is the field under test.
#[test]
fn text_fields_take_every_editing_key() {
    let home = temporary_home("keys2");
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mirrord-tui"));
    cmd.cwd(empty_dir());
    cmd.env("TERM", "xterm-256color");
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
    let mut send = |bytes: &[u8]| {
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    };

    wait_for(&rx, &mut parser, "<?> help");
    send(b":run\r");
    wait_for(&rx, &mut parser, "Which folders do you want to run?");
    let field = |text: &str| format!("\u{2315} {text} ");

    // Space types while searching.
    send(b"shop chk");
    wait_for(&rx, &mut parser, &field("shop chk"));
    // Home, Right twice: the right arrow moves the cursor rather than opening a folder.
    send(b"\x1b[H\x1b[C\x1b[CX");
    wait_for(&rx, &mut parser, &field("shXop chk"));
    // End, then Shift+Option+Left as most terminals send it selects the last word.
    send(b"\x1b[F\x1b[1;4Dcheck");
    wait_for(&rx, &mut parser, &field("shXop check"));
    // The same with Option marked the old "meta" way.
    send(b"\x1b[1;10Dchk");
    wait_for(&rx, &mut parser, &field("shXop chk"));
    // Option+Shift+Left as `Esc B`.
    send(b"\x1bBout");
    wait_for(&rx, &mut parser, &field("shXop out"));
    // Option+Backspace deletes the word, Ctrl+Z brings it back.
    send(b"\x1b\x7fY");
    wait_for(&rx, &mut parser, &field("shXop Y"));
    // Ctrl+Z steps back: first the typed `Y`, then the deleted word comes back.
    send(b"\x1a\x1a");
    wait_for(&rx, &mut parser, &field("shXop out"));
    // A double-click selects the word under it.
    let screen = wait_for(&rx, &mut parser, &field("shXop out"));
    let (row, column) = locate(&screen, "shXop");
    let click = format!(
        "\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m",
        x = column + 2,
        y = row + 1
    );
    send(click.as_bytes());
    send(click.as_bytes());
    send(b"shop");
    wait_for(&rx, &mut parser, &field("shop out"));
    // Cmd+A, as terminals that pass Cmd on send it, selects everything.
    send(b"\x1b[97;9upay");
    wait_for(&rx, &mut parser, &field("pay"));
    // Ctrl+A then Ctrl+X cuts it all, Ctrl+V pastes it back.
    send(b"\x01\x18");
    wait_for(&rx, &mut parser, "\u{2315} type to search");
    send(b"\x16");
    wait_for(&rx, &mut parser, &field("pay"));
    // Ctrl+W deletes the word before the cursor.
    send(b" now\x17X");
    wait_for(&rx, &mut parser, &field("pay X"));

    send(b"\x03");
    child.wait().unwrap();
}

/// `m` (or a click on the sidebar's switch) narrows every screen to the sessions running on this
/// machine, and the panels say so.
#[test]
fn only_my_sessions_switch_narrows_screens_and_says_so() {
    let home = temporary_home("mine");
    serve_session(
        &home,
        "3F2A9C01B4D5E6F7",
        SessionInfo {
            session_id: "3F2A9C01B4D5E6F7".to_owned(),
            key: Some("alice-feature".to_owned()),
            target: "deployment/api".to_owned(),
            namespace: Some("staging".to_owned()),
            context: None,
            started_at: k8s_openapi::jiff::Timestamp::now().to_string(),
            mirrord_version: "3.249.0".to_owned(),
            is_operator: true,
            processes: Vec::new(),
            port_subscriptions: Vec::new(),
            config: serde_json::Value::Null,
        },
    );

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mirrord-tui"));
    cmd.cwd(empty_dir());
    cmd.env("TERM", "xterm-256color");
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
    let mut send = |bytes: &[u8]| {
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    };

    // Off to begin with: everything shows.
    wait_for(&rx, &mut parser, "\u{25cb} everything");
    send(b":sessions\r");
    wait_for(&rx, &mut parser, "\u{256d} sessions");

    // `m` turns it on: the sidebar counts the session running here, and the panel says the list
    // is narrowed.
    send(b"m");
    wait_for(&rx, &mut parser, "\u{25c9} only mine \u{b7} 1");
    wait_for(&rx, &mut parser, "only mine \u{b7} updated");

    // A click on the switch turns it off again.
    let screen = wait_for(&rx, &mut parser, "only mine \u{b7} 1");
    let (row, column) = locate(&screen, "only mine \u{b7} 1");
    send(
        format!(
            "\x1b[<0;{};{}M\x1b[<0;{};{}m",
            column + 1,
            row + 1,
            column + 1,
            row + 1
        )
        .as_bytes(),
    );
    wait_for(&rx, &mut parser, "\u{25cb} everything");

    send(b"q");
    child.wait().unwrap();
}

/// A run's log says how to get back, Esc returns to the steps while it keeps running, and once
/// it stops Enter returns for good. `MIRRORD_BIN` points the run at a stand-in for mirrord.
#[test]
fn a_run_can_be_left_watched_again_and_closed() {
    let home = temporary_home("runlog");
    let project = home.join("checkout");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("package.json"),
        r#"{ "name": "checkout", "scripts": { "dev": "node server.js" } }"#,
    )
    .unwrap();
    // The folder's own config, which the config step uses without being asked.
    std::fs::create_dir_all(project.join(".mirrord")).unwrap();
    std::fs::write(
        project.join(".mirrord").join("mirrord.json"),
        r#"{ "feature": { "env": true } }"#,
    )
    .unwrap();
    let fake = home.join("fake-mirrord");
    std::fs::write(
        &fake,
        "#!/bin/sh\necho fake mirrord up is running\nexec sleep 60\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mirrord-tui"));
    cmd.cwd(&project);
    cmd.env("TERM", "xterm-256color");
    cmd.env("HOME", &home);
    cmd.env("MIRRORD_BIN", &fake);
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
    let mut send = |bytes: &[u8]| {
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    };

    wait_for(&rx, &mut parser, "What should they run against?");
    send(b"\r");
    wait_for(&rx, &mut parser, "the folder's own config");
    wait_for(
        &rx,
        &mut parser,
        "Start from  \u{2261} checkout/.mirrord/mirrord.json",
    );
    wait_for(&rx, &mut parser, "READY");
    send(b"\x12");

    // The log says what to do.
    wait_for(&rx, &mut parser, "fake mirrord up is running");
    wait_for(&rx, &mut parser, "esc  back to the steps (keeps running)");
    // Esc: back to the steps, and the step bar says it still runs.
    send(b"\x1b");
    wait_for(&rx, &mut parser, "running \u{b7} ctrl+l log");
    // Ctrl+L: the log again.
    send(b"\x0c");
    wait_for(&rx, &mut parser, "fake mirrord up is running");
    // x stops it; then Enter is the way back, and the steps are ready for another run.
    send(b"x");
    wait_for(&rx, &mut parser, "enter  back to the steps");
    send(b"\r");
    let screen = wait_for(&rx, &mut parser, "How should mirrord run them?");
    println!("{screen}");
    wait_for(&rx, &mut parser, "\u{25b6} run");

    // The run is remembered: back on the folders, it is first under "run again", and one Enter
    // sets it all up again on the config step.
    send(b"\x1b");
    wait_for(&rx, &mut parser, "What should they run against?");
    send(b"\x1b");
    wait_for(&rx, &mut parser, "RUN AGAIN");
    let screen = wait_for(&rx, &mut parser, "no target \u{b7} ");
    println!("{screen}");
    send(b"\r");
    wait_for(&rx, &mut parser, "set up like before - ctrl+r runs it");
    wait_for(&rx, &mut parser, "How should mirrord run them?");

    send(b"\x03");
    child.wait().unwrap();
}
