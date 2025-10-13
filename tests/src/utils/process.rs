use core::ops::Not;
#[cfg(not(target_os = "windows"))]
use std::os::unix::process::ExitStatusExt;
use std::{
    collections::HashMap,
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use fancy_regex::Regex;
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::RwLock,
    task::JoinHandle,
};

use crate::utils::format_time;

/// Wraps a bunch of things of a [`Child`] process, so we can check its output for errors/specific
/// messages.
///
/// It's mostly created by one of the [`super::run_command`] functions, such as
/// [`super::run_command::run_exec_with_target`], where we start a child test process running
/// `mirrord`, wait for it to finish and look into its `stdout/stderr`.
pub struct TestProcess {
    /// The [`Child`] process started, running `mirrord`.
    pub child: Child,
    /// `stderr` we use to check for `ERROR` logs/messages from the test app.
    stderr_data: Arc<RwLock<String>>,
    /// `stdout` we use to check for logs/messages from the test app.
    stdout_data: Arc<RwLock<String>>,
    /// Task that reads `stderr`.
    stderr_task: Option<JoinHandle<()>>,
    /// Task that reads `stdout`.
    stdout_task: Option<JoinHandle<()>>,
    /// `^.*ERROR[^\w_-]` [`Regex`] to look for our errors in test apps.
    error_capture: Regex,
    /// `WARN` [`Regex`] to look for our warnings in test apps.
    warn_capture: Regex,
    /// Keeps the temporary directory alive for the process's lifetime.
    _tempdir: Option<TempDir>,
}

impl TestProcess {
    pub async fn get_stdout(&self) -> String {
        self.stdout_data.read().await.clone()
    }

    pub async fn get_stderr(&self) -> String {
        self.stderr_data.read().await.clone()
    }

    pub async fn assert_log_level(&self, stderr: bool, level: &str) {
        if stderr {
            assert!(
                self.stderr_data.read().await.contains(level).not(),
                "application stderr should not contain `{level}`"
            );
        } else {
            assert!(
                self.stdout_data.read().await.contains(level).not(),
                "application stdout should not contain `{level}`"
            );
        }
    }

    pub async fn assert_python_fileops_stderr(&self) {
        assert!(
            self.stderr_data.read().await.contains("FAILED").not(),
            "application stderr should not contain `FAILED`"
        );
    }

    pub async fn wait_assert_success(&mut self) {
        let output = self.wait().await;
        #[cfg(not(target_os = "windows"))]
        assert!(
            output.success(),
            "application unexpectedly failed: exit code {:?}, signal code {:?}",
            output.code(),
            output.signal(),
        );

        #[cfg(target_os = "windows")]
        assert!(
            output.success(),
            "application unexpectedly failed: exit code {:?}",
            output.code(),
        );
    }

    pub async fn wait_assert_fail(&mut self) {
        let output = self.wait().await;
        #[cfg(not(target_os = "windows"))]
        assert!(
            output.success().not(),
            "application unexpectedly succeeded: exit code {:?}, signal code {:?}",
            output.code(),
            output.signal()
        );

        #[cfg(target_os = "windows")]
        assert!(
            output.success().not(),
            "application unexpectedly succeeded: exit code {:?}",
            output.code(),
        );
    }

    pub async fn assert_stdout_contains(&self, string: &str) {
        assert!(
            self.get_stdout().await.contains(string),
            "application stdout should contain `{string}`",
        );
    }

    pub async fn assert_stdout_doesnt_contain(&self, string: &str) {
        assert!(
            self.get_stdout().await.contains(string).not(),
            "application stdout should not contain `{string}`",
        );
    }

    pub async fn assert_stderr_contains(&self, string: &str) {
        assert!(
            self.get_stderr().await.contains(string),
            "application stderr should contain `{string}`",
        );
    }

    pub async fn assert_stderr_doesnt_contain(&self, string: &str) {
        assert!(
            self.get_stderr().await.contains(string).not(),
            "application stderr should not contain `{string}`",
        );
    }

    pub async fn assert_no_error_in_stdout(&self) {
        assert!(
            self.error_capture
                .is_match(&self.stdout_data.read().await)
                .unwrap()
                .not(),
            "application stdout contains an error"
        );
    }

    pub async fn assert_no_error_in_stderr(&self) {
        assert!(
            self.error_capture
                .is_match(&self.stderr_data.read().await)
                .unwrap()
                .not(),
            "application stderr contains an error"
        );
    }

    pub async fn assert_no_warn_in_stdout(&self) {
        assert!(
            self.warn_capture
                .is_match(&self.stdout_data.read().await)
                .unwrap()
                .not(),
            "application stdout contains a warning"
        );
    }

    pub async fn assert_no_warn_in_stderr(&self) {
        assert!(
            self.warn_capture
                .is_match(&self.stderr_data.read().await)
                .unwrap()
                .not(),
            "application stderr contains a warning"
        );
    }

    pub async fn wait_for_line(&self, timeout: Duration, line: &str) {
        let now = std::time::Instant::now();
        while now.elapsed() < timeout {
            let stderr = self.get_stderr().await;
            if stderr.contains(line) {
                return;
            }
            // avoid busyloop
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("Timeout waiting for line: {line}");
    }

    pub async fn wait_for_line_stdout(&self, timeout: Duration, line: &str) {
        let now = std::time::Instant::now();
        while now.elapsed() < timeout {
            let stdout = self.get_stdout().await;
            if stdout.contains(line) {
                return;
            }
            // avoid busyloop
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("Timeout waiting for line: {line}");
    }

    /// Wait for the test app to output (at least) the given amount of lines.
    ///
    /// # Arguments
    ///
    /// * `timeout` - how long to wait for process to output enough lines.
    ///
    /// # Panics
    /// If `timeout` has passed and stdout still does not contain `n` lines.
    pub async fn await_n_lines(&self, n: usize, timeout: Duration) -> Vec<String> {
        tokio::time::timeout(timeout, async move {
            loop {
                let stdout = self.get_stdout().await;
                if stdout.lines().count() >= n {
                    return stdout
                        .lines()
                        .map(ToString::to_string)
                        .collect::<Vec<String>>();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("Test process output did not produce expected amount of lines in time.")
    }

    /// Wait for the test app to output the given amount of lines, then assert it did not output
    /// more than expected.
    ///
    /// > Note: we do not wait to make sure more lines are not printed, we just check the lines we
    /// > got after waiting for n lines. So it is possible for this to happen:
    ///   1. Test process outputs `n` lines.
    ///   2. `await_n_lines` returns those `n` lines.
    ///   3. This function asserts there are only `n` lines.
    ///   3. The test process outputs more lines.
    ///
    /// # Arguments
    ///
    /// * `timeout` - how long to wait for process to output enough lines.
    ///
    /// # Panics
    /// - If `timeout` has passed and stdout still does not contain `n` lines.
    /// - If stdout contains more than `n` lines.
    pub async fn await_exactly_n_lines(&self, n: usize, timeout: Duration) -> Vec<String> {
        let lines = self.await_n_lines(n, timeout).await;
        assert_eq!(
            lines.len(),
            n,
            "Test application printed out more lines than expected."
        );
        lines
    }

    pub async fn write_to_stdin(&mut self, data: &[u8]) {
        match self.child.stdin {
            Some(ref mut stdin) => {
                stdin.write(data).await.unwrap();
            }
            _ => {
                panic!("Can't write to test app's stdin!");
            }
        }
    }

    /// Waits for process to end and stdout/err to be read. - returns final exit status
    pub async fn wait(&mut self) -> ExitStatus {
        eprintln!("waiting for process to exit");
        let exit_status = self.child.wait().await.unwrap();
        eprintln!("process exit, waiting for stdout/err to be read");
        self.stdout_task
            .take()
            .expect("can't call wait twice")
            .await
            .unwrap();
        self.stderr_task
            .take()
            .expect("can't call wait twice")
            .await
            .unwrap();
        eprintln!("stdout/err read and finished");
        exit_status
    }

    pub fn from_child(mut child: Child, tempdir: Option<TempDir>) -> TestProcess {
        let stderr_data = Arc::new(RwLock::new(String::new()));
        let stdout_data = Arc::new(RwLock::new(String::new()));
        let child_stderr = child.stderr.take().unwrap();
        let child_stdout = child.stdout.take().unwrap();
        let stderr_data_reader = stderr_data.clone();
        let stdout_data_reader = stdout_data.clone();
        let pid = child.id().unwrap();

        let stderr_task = Some(tokio::spawn(async move {
            let mut reader = BufReader::new(child_stderr);
            let mut buf = [0; 1024];
            loop {
                let n = reader.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }

                let string = String::from_utf8_lossy(&buf[..n]);
                eprintln!("stderr {} {pid}: {}", format_time(), string);
                {
                    stderr_data_reader.write().await.push_str(&string);
                }
            }
        }));
        let stdout_task = Some(tokio::spawn(async move {
            let mut reader = BufReader::new(child_stdout);
            let mut buf = [0; 1024];
            loop {
                let n = reader.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                let string = String::from_utf8_lossy(&buf[..n]);
                print!("stdout {} {pid}: {}", format_time(), string);
                {
                    stdout_data_reader.write().await.push_str(&string);
                }
            }
        }));

        let error_capture = Regex::new(r"^.*ERROR[^\w_-]").unwrap();
        let warn_capture = Regex::new(r"WARN").unwrap();

        TestProcess {
            child,
            error_capture,
            warn_capture,
            stderr_data,
            stdout_data,
            stderr_task,
            stdout_task,
            _tempdir: tempdir,
        }
    }

    pub async fn start_process(
        executable: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> TestProcess {
        println!("EXECUTING: {executable}");
        let child = Command::new(executable)
            .args(args)
            .envs(env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        println!("Started application.");
        TestProcess::from_child(child, None)
    }
}
