//! Hands the console over from mirrord's progress reporting to the user's process on Windows.
//!
//! On Unix, `mirrord exec` `execve`s into the user's binary, which kills every indicatif ticker
//! thread along with the CLI. On Windows the CLI stays alive to wait for the child, and the child
//! inherits our console. An indicatif spinner that is still alive at that point keeps redrawing
//! itself every tick: it moves the cursor up and clears as many lines as the progress tree
//! occupies, which erases whatever the user's program printed in the meantime. This holds for
//! every console mirrord can run in (conhost, and ConPTY-backed terminals such as Windows
//! Terminal, VS Code or JetBrains), since they all share one screen buffer between us and the
//! child.

use mirrord_progress::{Progress, ProgressTracker};

/// The [`Progress`] handed to
/// [`LayerManagedProcess::execute`](mirrord_layer_lib::process::windows::execution::LayerManagedProcess::execute)
/// by `mirrord exec`.
///
/// Finishing it finishes the whole progress tree (the task, then the root), and
/// `LayerManagedProcess` does that right before resuming the child's main thread, so the child
/// never writes to the console while a spinner is still alive.
pub(crate) struct ProcessHandoffProgress<'a, P> {
    pub(crate) task: P,
    /// Only the top-level handoff progress owns the root, subtasks get [`None`].
    pub(crate) root: Option<&'a mut ProgressTracker>,
}

impl<P: Progress> Progress for ProcessHandoffProgress<'_, P> {
    fn subtask(&self, text: &str) -> Self {
        Self {
            task: self.task.subtask(text),
            root: None,
        }
    }

    fn success(&mut self, msg: Option<&str>) {
        self.task.success(msg);
        if let Some(root) = self.root.take() {
            root.success(None);
        }
    }

    fn failure(&mut self, msg: Option<&str>) {
        self.task.failure(msg);
    }

    fn warning(&self, msg: &str) {
        self.task.warning(msg);
    }

    fn info(&self, msg: &str) {
        self.task.info(msg);
    }

    fn ide(&self, value: serde_json::Value) {
        self.task.ide(value);
    }

    fn print(&self, msg: &str) {
        self.task.print(msg);
    }

    fn add_to_print_buffer(&mut self, msg: &str) {
        self.task.add_to_print_buffer(msg);
    }

    fn set_fail_on_drop(&mut self, fail: bool) {
        self.task.set_fail_on_drop(fail);
    }

    fn suspend<F: FnOnce() -> R, R>(&self, f: F) -> R {
        self.task.suspend(f)
    }
}

/// These tests drive real spinners, so they need a real console: the test binary re-runs itself
/// inside a ConPTY, in one of the roles below, and inspects the rendered screen.
#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        process::Command,
        thread,
        time::Duration,
    };

    use mirrord_progress::{Progress, ProgressTracker, SpinnerProgress};
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    use super::ProcessHandoffProgress;

    /// Selects what a re-run of the test binary does, see [`run_role`].
    const ROLE_ENV: &str = "MIRRORD_TEST_PROCESS_HANDOFF_ROLE";

    /// Printed by the fake user process, must still be on screen when everything is done.
    const MARKER: &str = "process-handoff-child-marker";

    const ROWS: u16 = 50;
    const COLS: u16 = 160;

    /// ConPTY (started with `PSEUDOCONSOLE_INHERIT_CURSOR`) asks for the cursor position and
    /// waits for the answer before rendering anything.
    const CURSOR_POSITION_REQUEST: &[u8] = b"\x1b[6n";

    /// Name of the test that called this, as libtest expects it in `--exact` filters.
    fn test_filter(test_name: &str) -> String {
        let module = module_path!()
            .split_once("::")
            .map(|(_, module)| module)
            .unwrap_or(module_path!());
        format!("{module}::{test_name}")
    }

    /// The part of `mirrord exec` that runs inside the console: a progress tree like the one
    /// `exec_process` builds, the handoff `LayerManagedProcess` performs before `ResumeThread`,
    /// and a child process that inherits the console.
    fn run_role(role: &str, test_name: &str) {
        match role {
            "child" => {
                println!("{MARKER}");
                // Long enough for an unfinished spinner to tick over our output many times.
                thread::sleep(Duration::from_millis(1500));
            }
            "cli" | "cli-without-handoff" => {
                let mut root: ProgressTracker = SpinnerProgress::new("mirrord exec").into();
                let mut preparing = root.subtask("preparing to launch process");
                preparing.success(Some("ready to launch process"));
                let running = root.subtask("running process");

                if role == "cli" {
                    let mut handoff = ProcessHandoffProgress {
                        task: running,
                        root: Some(&mut root),
                    };
                    handoff.success(Some("Ready!"));
                } else {
                    let mut running = running;
                    running.success(Some("Ready!"));
                }

                let status = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        &test_filter(test_name),
                        "--include-ignored",
                        "--nocapture",
                    ])
                    .env(ROLE_ENV, "child")
                    .status()
                    .unwrap();
                assert!(status.success());
            }
            other => panic!("unknown role {other}"),
        }
    }

    /// Runs the test binary in the `role` inside a fresh ConPTY and returns the final screen.
    fn screen_after(role: &str, test_name: &str) -> String {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();

        let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            &test_filter(test_name),
            "--include-ignored",
            "--nocapture",
        ]);
        command.cwd(std::env::current_dir().unwrap());
        command.env(ROLE_ENV, role);
        // indicatif hides the spinners when `TERM=dumb`.
        command.env_remove("TERM");
        command.env_remove(mirrord_progress::MIRRORD_PROGRESS_ENV);

        let mut child = pty.slave.spawn_command(command).unwrap();
        drop(pty.slave);

        let mut reader = pty.master.try_clone_reader().unwrap();
        let mut writer = pty.master.take_writer().unwrap();
        let output = thread::spawn(move || {
            let mut output = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let Ok(read @ 1..) = reader.read(&mut buffer) else {
                    break output;
                };
                let chunk = buffer.get(..read).unwrap_or_default();
                if chunk
                    .windows(CURSOR_POSITION_REQUEST.len())
                    .any(|window| window == CURSOR_POSITION_REQUEST)
                {
                    let _ = writer.write_all(b"\x1b[1;1R");
                }
                output.extend_from_slice(chunk);
            }
        });

        let status = child.wait().unwrap();
        // Closing the pseudoconsole flushes the rest of its output and ends the reader.
        drop(pty.master);
        let output = output.join().unwrap();
        assert!(status.success(), "{role} role failed: {status:?}");

        let mut parser = vt100::Parser::new(ROWS, COLS, 0);
        parser.process(&output);
        parser.screen().contents()
    }

    fn marker_on_own_line(screen: &str) -> bool {
        screen.lines().any(|line| line.trim_end() == MARKER)
    }

    #[test]
    fn child_output_survives_progress_handoff() {
        const NAME: &str = "child_output_survives_progress_handoff";
        if let Ok(role) = std::env::var(ROLE_ENV) {
            return run_role(&role, NAME);
        }

        let screen = screen_after("cli", NAME);
        assert!(
            marker_on_own_line(&screen),
            "the child's output was overwritten:\n{screen}"
        );
    }

    /// The failure [`ProcessHandoffProgress`] prevents: with the root spinner still ticking, the
    /// child's output is wiped from the screen. Ignored because it depends on spinner timing, run
    /// it to check that [`child_output_survives_progress_handoff`] can tell the difference.
    #[test]
    #[ignore]
    fn child_output_is_overwritten_without_progress_handoff() {
        const NAME: &str = "child_output_is_overwritten_without_progress_handoff";
        if let Ok(role) = std::env::var(ROLE_ENV) {
            return run_role(&role, NAME);
        }

        let screen = screen_after("cli-without-handoff", NAME);
        assert!(
            !marker_on_own_line(&screen),
            "the child's output survived:\n{screen}"
        );
    }
}
